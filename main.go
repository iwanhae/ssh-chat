package main

import (
	"bufio"
	"context"
	"fmt"
	"log"
	"math/rand"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gliderlabs/ssh"
)

type Message struct {
	Time  time.Time
	Nick  string
	Text  string
	Color int
}

type ChatServer struct {
	mu       sync.RWMutex
	messages []Message
	clients  map[*Client]struct{}
}

var (
	globalChat   = NewChatServer()
	guestCounter uint64
)

func NewChatServer() *ChatServer {
	cs := &ChatServer{
		clients: make(map[*Client]struct{}),
	}
	welcome := Message{
		Time:  time.Now(),
		Nick:  "server",
		Text:  "Welcome to the SSH chat! Use ↑/↓ to scroll and Enter to send messages.",
		Color: 37,
	}
	cs.messages = append(cs.messages, welcome)
	cs.logMessage(welcome)
	return cs
}

func (cs *ChatServer) AddClient(c *Client) {
	cs.mu.Lock()
	cs.clients[c] = struct{}{}
	cs.mu.Unlock()
}

func (cs *ChatServer) RemoveClient(c *Client) {
	cs.mu.Lock()
	delete(cs.clients, c)
	cs.mu.Unlock()
}

func (cs *ChatServer) AppendMessage(msg Message) {
	cs.mu.Lock()
	cs.messages = append(cs.messages, msg)
	clients := make([]*Client, 0, len(cs.clients))
	for c := range cs.clients {
		clients = append(clients, c)
	}
	cs.mu.Unlock()

	cs.logMessage(msg)

	for _, client := range clients {
		client.Notify()
	}
}

func (cs *ChatServer) AppendSystemMessage(text string) {
	cs.AppendMessage(Message{
		Time:  time.Now(),
		Nick:  "server",
		Text:  text,
		Color: 37,
	})
}

func (cs *ChatServer) Messages() []Message {
	cs.mu.RLock()
	defer cs.mu.RUnlock()
	out := make([]Message, len(cs.messages))
	copy(out, cs.messages)
	return out
}

func (cs *ChatServer) ClientCount() int {
	cs.mu.RLock()
	defer cs.mu.RUnlock()
	return len(cs.clients)
}

func (cs *ChatServer) logMessage(msg Message) {
	sanitized := strings.ReplaceAll(msg.Text, "\n", "\\n")
	log.Printf("%s [%s] %s", msg.Time.Format(time.RFC3339), msg.Nick, sanitized)
}

type Client struct {
	session ssh.Session
	server  *ChatServer

	mu           sync.Mutex
	width        int
	height       int
	scrollOffset int
	inputBuffer  []rune

	updateCh  chan struct{}
	done      chan struct{}
	closeOnce sync.Once
	wg        sync.WaitGroup
	nickname  string
	color     int
}

var colors = []int{
	31, 32, 33, 34, 35, 36,
}

func NewClient(server *ChatServer, session ssh.Session, nickname string, width, height int) *Client {
	if width <= 0 {
		width = 80
	}
	if height <= 0 {
		height = 24
	}
	return &Client{
		session:     session,
		server:      server,
		width:       width,
		height:      height,
		updateCh:    make(chan struct{}, 16),
		done:        make(chan struct{}),
		nickname:    nickname,
		color:       colors[rand.Intn(len(colors))],
		inputBuffer: make([]rune, 0, 128),
	}
}

func (c *Client) Start(reader *bufio.Reader, ctx context.Context) {
	c.wg.Add(2)
	go func() {
		defer c.wg.Done()
		c.renderLoop()
	}()
	go func() {
		defer c.wg.Done()
		c.inputLoop(reader)
	}()
	go func() {
		select {
		case <-ctx.Done():
			c.Close()
		case <-c.done:
		}
	}()
	c.Notify()
}

func (c *Client) Wait() {
	c.wg.Wait()
}

func (c *Client) Close() {
	c.closeOnce.Do(func() {
		close(c.done)
	})
}

func (c *Client) Notify() {
	select {
	case c.updateCh <- struct{}{}:
	default:
	}
}

func (c *Client) SetWindowSize(width, height int) {
	c.mu.Lock()
	if width > 0 {
		c.width = width
	}
	if height > 0 {
		c.height = height
	}
	c.mu.Unlock()
	c.Notify()
}

func (c *Client) MonitorWindow(winCh <-chan ssh.Window) {
	for win := range winCh {
		c.SetWindowSize(win.Width, win.Height)
	}
	c.Close()
}

func (c *Client) renderLoop() {
	for {
		select {
		case <-c.updateCh:
			c.render()
		case <-c.done:
			return
		}
	}
}

func (c *Client) render() {
	messages := c.server.Messages()

	c.mu.Lock()
	width := c.width
	height := c.height
	scroll := c.scrollOffset
	inputCopy := append([]rune(nil), c.inputBuffer...)
	c.mu.Unlock()

	if width <= 0 {
		width = 80
	}
	if height <= 0 {
		height = 24
	}

	messageArea := height - 2
	if messageArea < 1 {
		messageArea = 1
	}

	lines := formatMessages(messages, width)
	totalLines := len(lines)
	maxOffset := 0
	if totalLines > messageArea {
		maxOffset = totalLines - messageArea
	}
	if scroll > maxOffset {
		scroll = maxOffset
		c.mu.Lock()
		c.scrollOffset = scroll
		c.mu.Unlock()
	}

	start := 0
	if totalLines > messageArea {
		start = totalLines - messageArea - scroll
		if start < 0 {
			start = 0
		}
	}
	end := start + messageArea
	if end > totalLines {
		end = totalLines
	}

	status := fmt.Sprintf("Users:%d Messages:%d Scroll:%d/%d ↑/↓ to scroll", c.server.ClientCount(), len(messages), scroll, maxOffset)
	status = fitString(status, width)

	inputText := string(inputCopy)
	inputLimit := width - 2
	if inputLimit < 1 {
		inputLimit = width
	}
	inputText = tailString(inputText, inputLimit)

	var b strings.Builder
	b.Grow((messageArea + 3) * (width + 8))
	b.WriteString("\x1b[?25l")
	b.WriteString("\x1b[H")

	for i := 0; i < messageArea; i++ {
		idx := start + i
		b.WriteString("\x1b[2K")
		if idx < end {
			b.WriteString(lines[idx])
		}
		b.WriteByte('\n')
	}

	b.WriteString("\x1b[2K")
	b.WriteString(status)
	b.WriteByte('\n')

	b.WriteString("\x1b[2K")
	b.WriteString("> ")
	b.WriteString(inputText)
	b.WriteString("\x1b[K")
	b.WriteString("\x1b[?25h")

	if _, err := c.session.Write([]byte(b.String())); err != nil {
		c.Close()
	}
}

func (c *Client) inputLoop(reader *bufio.Reader) {
	for {
		r, _, err := reader.ReadRune()
		if err != nil {
			c.Close()
			return
		}

		switch r {
		case '\r':
			c.handleEnter()
		case '\n':
		// ignore bare line feeds; carriage return already handled
		case 127, '\b':
			c.handleBackspace()
		case 3: // Ctrl+C
			c.Close()
			return
		case 4: // Ctrl+D
			c.Close()
			return
		case '\x1b':
			c.handleEscape(reader)
		default:
			if !isControlRune(r) {
				c.handleRune(r)
			}
		}
	}
}

func (c *Client) handleEnter() {
	c.mu.Lock()
	text := strings.TrimSpace(string(c.inputBuffer))
	c.inputBuffer = c.inputBuffer[:0]
	c.scrollOffset = 0
	c.mu.Unlock()
	c.Notify()

	if text == "" {
		return
	}

	c.server.AppendMessage(Message{
		Time:  time.Now(),
		Nick:  c.nickname,
		Text:  text,
		Color: c.color,
	})

	if strings.Contains(text, "스프링") {
		c.server.AppendSystemMessage("물러가라 이 사악한 스프링놈아.")
	}

	if strings.Contains(text, "exit") {
		c.server.AppendSystemMessage("exit 안되요. 그냥 ctrl + c 하시죠")
	}

	if strings.Contains(text, "help") {
		c.server.AppendSystemMessage("help? 인생은 실전이에요.")
	}
}

func (c *Client) handleBackspace() {
	c.mu.Lock()
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = c.inputBuffer[:len(c.inputBuffer)-1]
	}
	c.mu.Unlock()
	c.Notify()
}

func (c *Client) handleRune(r rune) {
	c.mu.Lock()
	c.inputBuffer = append(c.inputBuffer, r)
	c.mu.Unlock()
	c.Notify()
}

func (c *Client) handleEscape(reader *bufio.Reader) {
	b1, err := reader.ReadByte()
	if err != nil {
		c.Close()
		return
	}
	if b1 != '[' {
		return
	}
	b2, err := reader.ReadByte()
	if err != nil {
		c.Close()
		return
	}
	switch b2 {
	case 'A':
		c.mu.Lock()
		c.scrollOffset++
		c.mu.Unlock()
		c.Notify()
	case 'B':
		c.mu.Lock()
		if c.scrollOffset > 0 {
			c.scrollOffset--
		}
		c.mu.Unlock()
		c.Notify()
	}
}

func isControlRune(r rune) bool {
	return r < 32 || r == 127
}

func formatMessages(messages []Message, width int) []string {
	if width <= 0 {
		width = 80
	}
	lines := make([]string, 0, len(messages))
	for _, msg := range messages {
		color := msg.Color
		if color == 0 {
			color = 37 // default to white
		}
		coloredNick := fmt.Sprintf("\x1b[%dm%s\x1b[0m", color, msg.Nick)
		prefix := fmt.Sprintf("[%s] %s: ", msg.Time.Format("15:04:05"), coloredNick)
		indent := strings.Repeat(" ", len(msg.Nick)+13)
		segments := strings.Split(msg.Text, "\n")
		for i, segment := range segments {
			base := segment
			if i == 0 {
				base = prefix + segment
			} else {
				base = indent + segment
			}
			wrapped := wrapString(base, width)
			lines = append(lines, wrapped...)
		}
	}
	if len(lines) == 0 {
		return []string{"(start chatting!)"}
	}
	return lines
}

func wrapString(s string, width int) []string {
	if width <= 0 {
		width = 80
	}
	runes := []rune(s)
	if len(runes) == 0 {
		return []string{""}
	}
	var result []string
	for len(runes) > 0 {
		if len(runes) <= width {
			result = append(result, string(runes))
			break
		}
		result = append(result, string(runes[:width]))
		runes = runes[width:]
	}
	return result
}

func fitString(s string, width int) string {
	if width <= 0 {
		return s
	}
	runes := []rune(s)
	if len(runes) <= width {
		return s
	}
	return string(runes[:width])
}

func tailString(s string, width int) string {
	if width <= 0 {
		return s
	}
	runes := []rune(s)
	if len(runes) <= width {
		return s
	}
	return string(runes[len(runes)-width:])
}

func generateGuestNickname() string {
	id := atomic.AddUint64(&guestCounter, 1)
	return fmt.Sprintf("guest-%d", id)
}

func main() {
	ssh.Handle(func(s ssh.Session) {
		ptyReq, winCh, isPty := s.Pty()
		if !isPty {
			fmt.Fprintln(s, "Error: PTY required. Reconnect with -t option.")
			s.Exit(1)
			return
		}

		reader := bufio.NewReader(s)
		nickname := strings.TrimSpace(s.User())
		if nickname == "" {
			nickname = generateGuestNickname()
		}

		client := NewClient(globalChat, s, nickname, int(ptyReq.Window.Width), int(ptyReq.Window.Height))
		globalChat.AddClient(client)

		defer func() {
			globalChat.RemoveClient(client)
			client.Close()
			globalChat.AppendSystemMessage(fmt.Sprintf("%s left the chat", nickname))
		}()

		fmt.Fprint(s, "\x1b[2J\x1b[H")
		globalChat.AppendSystemMessage(fmt.Sprintf("%s joined the chat", nickname))

		go client.MonitorWindow(winCh)

		client.Start(reader, s.Context())
		client.Wait()
	})

	log.Println("starting ssh chat server on port 2222...")
	log.Fatal(ssh.ListenAndServe(":2222", nil, ssh.HostKeyFile("host.key")))
}
