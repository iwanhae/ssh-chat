use crate::config::{ThreatAction, ThreatListFormat, ThreatListsConfig};
use ipnetwork::IpNetwork;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

pub struct ThreatListManager {
    config: ThreatListsConfig,
    ip_list: Arc<RwLock<HashSet<IpAddr>>>,
    cidr_list: Arc<RwLock<Vec<IpNetwork>>>,
}

impl ThreatListManager {
    pub fn new(config: ThreatListsConfig) -> Self {
        Self {
            config,
            ip_list: Arc::new(RwLock::new(HashSet::new())),
            cidr_list: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn start_auto_update(self: Arc<Self>) {
        if !self.config.enabled {
            return;
        }

        let update_interval = Duration::from_secs(self.config.update_interval_hours * 3600);

        // Initial update
        self.update_all().await;

        // Periodic updates
        tokio::spawn(async move {
            let mut interval = time::interval(update_interval);
            loop {
                interval.tick().await;
                self.update_all().await;
            }
        });
    }

    async fn update_all(&self) {
        for source in &self.config.sources {
            if !source.enabled {
                continue;
            }

            match self.fetch_list(source).await {
                Ok((ips, cidrs)) => {
                    let mut ip_list = self.ip_list.write();
                    let mut cidr_list = self.cidr_list.write();

                    // Clear existing entries to prevent unbounded growth
                    ip_list.clear();
                    cidr_list.clear();

                    for ip in ips {
                        ip_list.insert(ip);
                    }
                    for cidr in cidrs {
                        cidr_list.push(cidr);
                    }

                    println!(
                        "Updated threat list '{}': {} IPs, {} CIDRs",
                        source.name,
                        ip_list.len(),
                        cidr_list.len()
                    );
                }
                Err(e) => {
                    eprintln!("Failed to update threat list '{}': {}", source.name, e);
                }
            }
        }
    }

    async fn fetch_list(
        &self,
        source: &crate::config::ThreatListSource,
    ) -> Result<(Vec<IpAddr>, Vec<IpNetwork>), String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("HTTP client error: {}", e))?;

        let mut request = client.get(&source.url);

        // Add custom headers
        for (key, value) in &source.headers {
            request = request.header(key, value);
        }

        // Add query parameters
        for (key, value) in &source.params {
            request = request.query(&[(key, value)]);
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        let body = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        self.parse_list(&body, source.format)
    }

    fn parse_list(
        &self,
        content: &str,
        format: ThreatListFormat,
    ) -> Result<(Vec<IpAddr>, Vec<IpNetwork>), String> {
        let mut ips = Vec::new();
        let mut cidrs = Vec::new();

        match format {
            ThreatListFormat::Ip => {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                        continue;
                    }

                    if let Ok(ip) = line.parse::<IpAddr>() {
                        ips.push(ip);
                    }
                }
            }
            ThreatListFormat::Cidr => {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                        continue;
                    }

                    // Extract IP/CIDR from DROP format (e.g., "1.2.3.0/24 ; SBL123")
                    let parts: Vec<&str> = line.split(';').collect();
                    let cidr_str = parts[0].trim();

                    if let Ok(cidr) = cidr_str.parse::<IpNetwork>() {
                        cidrs.push(cidr);
                    }
                }
            }
            ThreatListFormat::Json => {
                // Simple JSON array of IPs
                if let Ok(json_ips) = serde_json::from_str::<Vec<String>>(content) {
                    for ip_str in json_ips {
                        if let Ok(ip) = ip_str.parse::<IpAddr>() {
                            ips.push(ip);
                        }
                    }
                }
            }
        }

        Ok((ips, cidrs))
    }

    pub fn check_ip(&self, ip: IpAddr) -> Result<(), String> {
        if !self.config.enabled {
            return Ok(());
        }

        // Check exact IP match
        {
            let ip_list = self.ip_list.read();
            if ip_list.contains(&ip) {
                let message = format!("IP {} is on threat list", ip);
                return match self.config.action {
                    ThreatAction::Block => Err(message),
                    ThreatAction::LogOnly => {
                        println!("⚠️  {}", message);
                        Ok(())
                    }
                };
            }
        }

        // Check CIDR match
        {
            let cidr_list = self.cidr_list.read();
            for network in cidr_list.iter() {
                if network.contains(ip) {
                    let message = format!("IP {} matches CIDR {} on threat list", ip, network);
                    return match self.config.action {
                        ThreatAction::Block => Err(message),
                        ThreatAction::LogOnly => {
                            println!("⚠️  {}", message);
                            Ok(())
                        }
                    };
                }
            }
        }

        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::path::PathBuf;

    #[test]
    fn test_threat_manager_disabled() {
        let config = ThreatListsConfig {
            enabled: false,
            update_interval_hours: 24,
            cache_dir: PathBuf::from("cache"),
            action: ThreatAction::Block,
            sources: vec![],
        };

        let manager = ThreatListManager::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
        assert!(manager.check_ip(ip).is_ok());
    }

    #[test]
    fn test_parse_ip_list() {
        let config = ThreatListsConfig {
            enabled: true,
            update_interval_hours: 24,
            cache_dir: PathBuf::from("cache"),
            action: ThreatAction::Block,
            sources: vec![],
        };

        let manager = ThreatListManager::new(config);
        let content = "# Comment\n1.2.3.4\n5.6.7.8\n\n; Another comment\n9.10.11.12";
        let (ips, cidrs) = manager.parse_list(content, ThreatListFormat::Ip).unwrap();

        assert_eq!(ips.len(), 3);
        assert_eq!(cidrs.len(), 0);
    }

    #[test]
    fn test_parse_cidr_list() {
        let config = ThreatListsConfig {
            enabled: true,
            update_interval_hours: 24,
            cache_dir: PathBuf::from("cache"),
            action: ThreatAction::Block,
            sources: vec![],
        };

        let manager = ThreatListManager::new(config);
        let content = "1.2.3.0/24 ; SBL123\n5.6.7.0/24 ; SBL456";
        let (ips, cidrs) = manager.parse_list(content, ThreatListFormat::Cidr).unwrap();

        assert_eq!(ips.len(), 0);
        assert_eq!(cidrs.len(), 2);
    }
}
