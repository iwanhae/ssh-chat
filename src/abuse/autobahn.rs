use crate::config::AutoBahnConfig;
use dashmap::DashMap;
use rand::Rng;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
struct ViolationRecord {
    count: u8,
    last_violation: SystemTime,
    connection_attempts: u32,
    last_connection_attempt: SystemTime,
}

pub struct AutoBahn {
    config: AutoBahnConfig,
    violations: Arc<DashMap<IpAddr, ViolationRecord>>,
}

impl AutoBahn {
    pub fn new(config: AutoBahnConfig) -> Self {
        Self {
            config,
            violations: Arc::new(DashMap::new()),
        }
    }

    pub fn record_violation(&self, ip: IpAddr) {
        if !self.config.enabled {
            return;
        }

        self.violations
            .entry(ip)
            .and_modify(|record| {
                record.count = record.count.saturating_add(1);
                record.last_violation = SystemTime::now();
            })
            .or_insert(ViolationRecord {
                count: 1,
                last_violation: SystemTime::now(),
                connection_attempts: 0,
                last_connection_attempt: SystemTime::now(),
            });
    }

    pub fn record_connection_attempt(&self, ip: IpAddr) {
        if !self.config.enabled {
            return;
        }

        self.violations
            .entry(ip)
            .and_modify(|record| {
                record.connection_attempts = record.connection_attempts.saturating_add(1);
                record.last_connection_attempt = SystemTime::now();
            })
            .or_insert(ViolationRecord {
                count: 0,
                last_violation: SystemTime::now(),
                connection_attempts: 1,
                last_connection_attempt: SystemTime::now(),
            });
    }

    pub async fn check_connection(&self, ip: IpAddr) -> Result<(), String> {
        if !self.config.enabled {
            return Ok(());
        }

        self.record_connection_attempt(ip);

        if let Some(record) = self.violations.get(&ip) {
            let record = record.value();

            // Apply exponential connection delay
            let delay_ms = self.calculate_connection_delay(record.connection_attempts);
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            // Check if challenge is required
            if record.count >= self.config.challenge_after_violations {
                return self.require_challenge(ip, record.count).await;
            }

            // Apply progressive delays based on violation count
            let delay_ms = match record.count {
                0 => 0,
                1 => self.config.delay_on_first_violation,
                2 => self.config.delay_on_second_violation,
                3 => self.config.delay_on_third_violation,
                _ => self.config.delay_on_fourth_violation,
            };

            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }

        Ok(())
    }

    fn calculate_connection_delay(&self, attempts: u32) -> u64 {
        if attempts <= 1 {
            return 0;
        }

        let base_delay = self.config.connection_delay_base_ms as f64;
        let multiplier = self.config.connection_delay_multiplier;
        let max_delay = self.config.connection_delay_max_ms;

        let delay = base_delay * multiplier.powi((attempts - 1) as i32);
        delay.min(max_delay as f64) as u64
    }

    async fn require_challenge(&self, ip: IpAddr, violation_count: u8) -> Result<(), String> {
        // Generate simple math challenge
        let mut rng = rand::rng();
        let a = rng.random_range(10..100);
        let b = rng.random_range(10..100);
        let _expected_answer = a + b;

        println!(
            "🔒 AutoBahn challenge for {} (violations: {}): {} + {} = ?",
            ip, violation_count, a, b
        );

        // In real implementation, this would send the challenge to the client
        // and wait for response. For now, we simulate a timeout.
        tokio::time::sleep(Duration::from_secs(self.config.challenge_timeout_seconds)).await;

        // Simulate challenge failure
        Err(format!(
            "AutoBahn challenge required (violations: {})",
            violation_count
        ))
    }

    pub fn get_violation_count(&self, ip: IpAddr) -> u8 {
        self.violations
            .get(&ip)
            .map(|record| record.count)
            .unwrap_or(0)
    }

    pub fn clear_violations(&self, ip: IpAddr) {
        self.violations.remove(&ip);
    }

    pub fn cleanup_old_records(&self, max_age: Duration) {
        let now = SystemTime::now();
        self.violations.retain(|_, record| {
            now.duration_since(record.last_violation)
                .unwrap_or(Duration::ZERO)
                < max_age
        });
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn create_test_config() -> AutoBahnConfig {
        AutoBahnConfig {
            enabled: true,
            delay_on_first_violation: 100,
            delay_on_second_violation: 500,
            delay_on_third_violation: 2000,
            delay_on_fourth_violation: 5000,
            challenge_after_violations: 3,
            challenge_timeout_seconds: 30,
            connection_delay_base_ms: 100,
            connection_delay_multiplier: 2.0,
            connection_delay_max_ms: 60000,
        }
    }

    #[test]
    fn test_violation_tracking() {
        let config = create_test_config();
        let autobahn = AutoBahn::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));

        assert_eq!(autobahn.get_violation_count(ip), 0);

        autobahn.record_violation(ip);
        assert_eq!(autobahn.get_violation_count(ip), 1);

        autobahn.record_violation(ip);
        assert_eq!(autobahn.get_violation_count(ip), 2);
    }

    #[test]
    fn test_clear_violations() {
        let config = create_test_config();
        let autobahn = AutoBahn::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));

        autobahn.record_violation(ip);
        assert_eq!(autobahn.get_violation_count(ip), 1);

        autobahn.clear_violations(ip);
        assert_eq!(autobahn.get_violation_count(ip), 0);
    }

    #[test]
    fn test_connection_delay_calculation() {
        let config = create_test_config();
        let autobahn = AutoBahn::new(config);

        assert_eq!(autobahn.calculate_connection_delay(0), 0);
        assert_eq!(autobahn.calculate_connection_delay(1), 0);
        assert_eq!(autobahn.calculate_connection_delay(2), 200); // 100 * 2^1 = 200
        assert_eq!(autobahn.calculate_connection_delay(3), 400); // 100 * 2^2 = 400
        assert_eq!(autobahn.calculate_connection_delay(4), 800); // 100 * 2^3 = 800
    }

    #[test]
    fn test_disabled_autobahn() {
        let mut config = create_test_config();
        config.enabled = false;
        let autobahn = AutoBahn::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));

        autobahn.record_violation(ip);
        assert_eq!(autobahn.get_violation_count(ip), 0);
    }
}
