use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Ban entry with expiration support
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanEntry {
    pub ip: IpAddr,
    pub reason: String,
    pub banned_at: SystemTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<SystemTime>,
}

impl BanEntry {
    /// Check if ban is still active
    pub fn is_active(&self) -> bool {
        match self.expires_at {
            None => true, // Permanent ban
            Some(expiry) => SystemTime::now() < expiry,
        }
    }

    /// Check if ban is expired
    pub fn is_expired(&self) -> bool {
        !self.is_active()
    }
}

/// Ban list storage
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BanList {
    bans: HashMap<IpAddr, BanEntry>,
}

/// Thread-safe ban manager
pub struct BanManager {
    ban_list_path: PathBuf,
    bans: Arc<RwLock<BanList>>,
}

impl BanManager {
    /// Create new BanManager and load existing bans
    pub fn new(ban_list_path: PathBuf) -> anyhow::Result<Self> {
        let ban_list = if ban_list_path.exists() {
            let content = fs::read_to_string(&ban_list_path)?;
            serde_json::from_str(&content)?
        } else {
            BanList::default()
        };

        let manager = Self {
            ban_list_path,
            bans: Arc::new(RwLock::new(ban_list)),
        };

        // Cleanup expired bans on startup
        manager.cleanup_expired();

        Ok(manager)
    }

    /// Add permanent ban
    pub fn ban_permanent(&self, ip: IpAddr, reason: String) -> anyhow::Result<()> {
        let entry = BanEntry {
            ip,
            reason,
            banned_at: SystemTime::now(),
            expires_at: None,
        };

        {
            let mut bans = self.bans.write();
            bans.bans.insert(ip, entry);
        }

        self.save()?;
        Ok(())
    }

    /// Add temporary ban
    pub fn ban_temporary(
        &self,
        ip: IpAddr,
        duration: Duration,
        reason: String,
    ) -> anyhow::Result<()> {
        let now = SystemTime::now();
        let expires_at = now + duration;

        let entry = BanEntry {
            ip,
            reason,
            banned_at: now,
            expires_at: Some(expires_at),
        };

        {
            let mut bans = self.bans.write();
            bans.bans.insert(ip, entry);
        }

        self.save()?;
        Ok(())
    }

    /// Remove ban
    pub fn unban(&self, ip: IpAddr) -> anyhow::Result<bool> {
        let removed = {
            let mut bans = self.bans.write();
            bans.bans.remove(&ip).is_some()
        };

        if removed {
            self.save()?;
        }

        Ok(removed)
    }

    /// Check if IP is banned (and ban is still active)
    pub fn is_banned(&self, ip: IpAddr) -> bool {
        let bans = self.bans.read();
        bans.bans
            .get(&ip)
            .map(|entry| entry.is_active())
            .unwrap_or(false)
    }

    /// Get ban details if IP is banned
    pub fn check_ban(&self, ip: IpAddr) -> Option<BanEntry> {
        let bans = self.bans.read();
        bans.bans
            .get(&ip)
            .filter(|entry| entry.is_active())
            .cloned()
    }

    /// Remove expired temporary bans
    pub fn cleanup_expired(&self) {
        let mut removed = false;

        {
            let mut bans = self.bans.write();
            let expired_ips: Vec<IpAddr> = bans
                .bans
                .iter()
                .filter(|(_, entry)| entry.is_expired())
                .map(|(ip, _)| *ip)
                .collect();

            for ip in expired_ips {
                bans.bans.remove(&ip);
                removed = true;
            }
        }

        if removed {
            let _ = self.save();
        }
    }

    /// Get all active bans
    pub fn get_all_bans(&self) -> Vec<BanEntry> {
        let bans = self.bans.read();
        bans.bans
            .values()
            .filter(|entry| entry.is_active())
            .cloned()
            .collect()
    }

    /// Get ban count
    pub fn ban_count(&self) -> usize {
        let bans = self.bans.read();
        bans.bans.values().filter(|e| e.is_active()).count()
    }

    /// Save bans to disk
    fn save(&self) -> anyhow::Result<()> {
        let bans = self.bans.read();
        let json = serde_json::to_string_pretty(&*bans)?;
        fs::write(&self.ban_list_path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn create_test_manager() -> (BanManager, PathBuf) {
        let path = PathBuf::from("/tmp/test_bans.json");
        let _ = fs::remove_file(&path); // Clean up from previous tests
        let manager = BanManager::new(path.clone()).unwrap();
        (manager, path)
    }

    #[test]
    fn test_permanent_ban() {
        let (manager, path) = create_test_manager();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        manager.ban_permanent(ip, "test ban".to_string()).unwrap();

        assert!(manager.is_banned(ip));
        assert_eq!(manager.ban_count(), 1);

        let entry = manager.check_ban(ip).unwrap();
        assert_eq!(entry.reason, "test ban");
        assert!(entry.expires_at.is_none()); // Permanent

        // Cleanup
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_temporary_ban() {
        let (manager, path) = create_test_manager();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

        manager
            .ban_temporary(ip, Duration::from_secs(3600), "temp ban".to_string())
            .unwrap();

        assert!(manager.is_banned(ip));

        let entry = manager.check_ban(ip).unwrap();
        assert_eq!(entry.reason, "temp ban");
        assert!(entry.expires_at.is_some());

        // Cleanup
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_unban() {
        let (manager, path) = create_test_manager();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3));

        manager.ban_permanent(ip, "test".to_string()).unwrap();
        assert!(manager.is_banned(ip));

        let removed = manager.unban(ip).unwrap();
        assert!(removed);
        assert!(!manager.is_banned(ip));

        // Cleanup
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_persistence() {
        let path = PathBuf::from("/tmp/test_bans_persist.json");
        let _ = fs::remove_file(&path);

        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4));

        // Create manager, add ban, drop
        {
            let manager = BanManager::new(path.clone()).unwrap();
            manager
                .ban_permanent(ip, "persist test".to_string())
                .unwrap();
        }

        // Create new manager, should load ban
        {
            let manager = BanManager::new(path.clone()).unwrap();
            assert!(manager.is_banned(ip));
            let entry = manager.check_ban(ip).unwrap();
            assert_eq!(entry.reason, "persist test");
        }

        // Cleanup
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_expired_ban_cleanup() {
        let (manager, path) = create_test_manager();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5));

        // Create ban that expired 1 second ago
        let entry = BanEntry {
            ip,
            reason: "expired".to_string(),
            banned_at: SystemTime::now() - Duration::from_secs(10),
            expires_at: Some(SystemTime::now() - Duration::from_secs(1)),
        };

        {
            let mut bans = manager.bans.write();
            bans.bans.insert(ip, entry);
        }

        assert!(!manager.is_banned(ip)); // Should return false (expired)

        manager.cleanup_expired();
        assert_eq!(manager.ban_count(), 0);

        // Cleanup
        let _ = fs::remove_file(path);
    }
}
