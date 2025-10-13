use crate::config::{FloodConfig, RateLimitConfig};
use dashmap::DashMap;
use governor::{Quota, RateLimiter as GovernorRateLimiter};
use nonzero_ext::nonzero;
use std::collections::VecDeque;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

type ClientRateLimiter = GovernorRateLimiter<
    governor::state::direct::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

pub struct RateLimiter {
    rate_config: RateLimitConfig,
    flood_config: FloodConfig,
    // Per-client rate limiters
    client_limiters: Arc<DashMap<Uuid, Arc<ClientRateLimiter>>>,
    // Per-IP connection tracking
    ip_connections: Arc<DashMap<IpAddr, Vec<Uuid>>>,
    // Flood detection: track message timestamps per client
    message_history: Arc<DashMap<Uuid, VecDeque<SystemTime>>>,
}

impl RateLimiter {
    pub fn new(rate_config: RateLimitConfig, flood_config: FloodConfig) -> Self {
        Self {
            rate_config,
            flood_config,
            client_limiters: Arc::new(DashMap::new()),
            ip_connections: Arc::new(DashMap::new()),
            message_history: Arc::new(DashMap::new()),
        }
    }

    pub fn register_client(&self, client_id: Uuid, ip: IpAddr) -> Result<(), String> {
        // Check connection limit per IP
        let connection_count = self
            .ip_connections
            .entry(ip)
            .or_default()
            .len();

        if connection_count >= self.flood_config.max_connections_per_ip {
            return Err(format!(
                "Too many connections from {} (max {})",
                ip, self.flood_config.max_connections_per_ip
            ));
        }

        // Add client to IP tracking
        self.ip_connections
            .entry(ip)
            .or_default()
            .push(client_id);

        // Create rate limiter for this client
        let quota = Quota::per_second(
            NonZeroU32::new(self.rate_config.messages_per_second as u32)
                .unwrap_or(nonzero!(1u32)),
        )
        .allow_burst(
            NonZeroU32::new(self.rate_config.burst_capacity as u32).unwrap_or(nonzero!(1u32)),
        );

        let limiter = Arc::new(GovernorRateLimiter::direct(quota));
        self.client_limiters.insert(client_id, limiter);

        // Initialize message history
        self.message_history.insert(client_id, VecDeque::new());

        Ok(())
    }

    pub fn unregister_client(&self, client_id: Uuid, ip: IpAddr) {
        // Remove from IP tracking
        if let Some(mut connections) = self.ip_connections.get_mut(&ip) {
            connections.retain(|id| *id != client_id);
            if connections.is_empty() {
                drop(connections);
                self.ip_connections.remove(&ip);
            }
        }

        // Remove rate limiter
        self.client_limiters.remove(&client_id);

        // Remove message history
        self.message_history.remove(&client_id);
    }

    pub fn check_rate_limit(&self, client_id: Uuid) -> Result<(), String> {
        if let Some(limiter) = self.client_limiters.get(&client_id) {
            match limiter.check() {
                Ok(_) => Ok(()),
                Err(_) => Err("Rate limit exceeded".to_string()),
            }
        } else {
            Err("Client not registered".to_string())
        }
    }

    pub fn check_flood(&self, client_id: Uuid) -> Result<(), String> {
        let now = SystemTime::now();
        let window = Duration::from_secs(self.flood_config.window_seconds);

        let mut history = self
            .message_history
            .get_mut(&client_id)
            .ok_or("Client not registered")?;

        // Remove old timestamps outside the window
        while let Some(&oldest) = history.front() {
            if now.duration_since(oldest).unwrap_or(Duration::ZERO) > window {
                history.pop_front();
            } else {
                break;
            }
        }

        // Check if flood limit exceeded
        if history.len() >= self.flood_config.max_messages_in_window {
            return Err(format!(
                "Flood detected: {} messages in {} seconds",
                history.len(),
                self.flood_config.window_seconds
            ));
        }

        // Add current message timestamp
        history.push_back(now);

        Ok(())
    }

    pub fn get_connection_count(&self, ip: IpAddr) -> usize {
        self.ip_connections
            .get(&ip)
            .map(|connections| connections.len())
            .unwrap_or(0)
    }

    pub fn cleanup_inactive_clients(&self, inactive_threshold: Duration) {
        let now = SystemTime::now();

        // Cleanup message history for inactive clients
        self.message_history.retain(|_, history| {
            if let Some(&last_message) = history.back() {
                now.duration_since(last_message)
                    .unwrap_or(Duration::ZERO)
                    < inactive_threshold
            } else {
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn create_test_configs() -> (RateLimitConfig, FloodConfig) {
        let rate_config = RateLimitConfig {
            messages_per_second: 2.0,
            burst_capacity: 5,
        };

        let flood_config = FloodConfig {
            window_seconds: 10,
            max_messages_in_window: 20,
            max_connections_per_ip: 3,
        };

        (rate_config, flood_config)
    }

    #[test]
    fn test_register_client() {
        let (rate_config, flood_config) = create_test_configs();
        let limiter = RateLimiter::new(rate_config, flood_config);

        let client_id = Uuid::new_v4();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        assert!(limiter.register_client(client_id, ip).is_ok());
        assert_eq!(limiter.get_connection_count(ip), 1);
    }

    #[test]
    fn test_connection_limit() {
        let (rate_config, flood_config) = create_test_configs();
        let limiter = RateLimiter::new(rate_config, flood_config);

        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Register up to max connections
        for _ in 0..3 {
            let client_id = Uuid::new_v4();
            assert!(limiter.register_client(client_id, ip).is_ok());
        }

        // Next connection should fail
        let client_id = Uuid::new_v4();
        assert!(limiter.register_client(client_id, ip).is_err());
    }

    #[test]
    fn test_unregister_client() {
        let (rate_config, flood_config) = create_test_configs();
        let limiter = RateLimiter::new(rate_config, flood_config);

        let client_id = Uuid::new_v4();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        limiter.register_client(client_id, ip).unwrap();
        assert_eq!(limiter.get_connection_count(ip), 1);

        limiter.unregister_client(client_id, ip);
        assert_eq!(limiter.get_connection_count(ip), 0);
    }

    #[test]
    fn test_rate_limit() {
        let (rate_config, flood_config) = create_test_configs();
        let limiter = RateLimiter::new(rate_config, flood_config);

        let client_id = Uuid::new_v4();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        limiter.register_client(client_id, ip).unwrap();

        // First few messages should succeed (within burst)
        for _ in 0..5 {
            assert!(limiter.check_rate_limit(client_id).is_ok());
        }

        // Next message should be rate limited
        assert!(limiter.check_rate_limit(client_id).is_err());
    }

    #[test]
    fn test_flood_detection() {
        let (rate_config, mut flood_config) = create_test_configs();
        flood_config.max_messages_in_window = 3;
        let limiter = RateLimiter::new(rate_config, flood_config);

        let client_id = Uuid::new_v4();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        limiter.register_client(client_id, ip).unwrap();

        // First 3 messages should succeed
        for _ in 0..3 {
            assert!(limiter.check_flood(client_id).is_ok());
        }

        // 4th message should trigger flood detection
        assert!(limiter.check_flood(client_id).is_err());
    }
}
