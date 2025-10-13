use crate::config::{GeoIpConfig, GeoIpMode};
use maxminddb::{geoip2, MaxMindDBError, Reader};
use std::net::IpAddr;
use std::sync::Arc;

pub struct GeoIpFilter {
    config: GeoIpConfig,
    reader: Option<Arc<Reader<Vec<u8>>>>,
}

impl GeoIpFilter {
    pub fn new(config: GeoIpConfig) -> Result<Self, MaxMindDBError> {
        let reader = if config.enabled {
            let reader = Reader::open_readfile(&config.database_path)?;
            Some(Arc::new(reader))
        } else {
            None
        };

        Ok(Self { config, reader })
    }

    pub fn check_ip(&self, ip: IpAddr) -> Result<(), String> {
        if !self.config.enabled {
            return Ok(());
        }

        let reader = self.reader.as_ref().ok_or("GeoIP reader not initialized")?;

        let country: geoip2::Country = reader
            .lookup(ip)
            .map_err(|e| format!("GeoIP lookup failed: {}", e))?;

        let iso_code = country
            .country
            .and_then(|c| c.iso_code)
            .ok_or("Country code not found")?;

        match self.config.mode {
            GeoIpMode::Blacklist => {
                if self.config.blocked_countries.contains(&iso_code.to_string()) {
                    return Err(self.config.rejection_message.clone());
                }
            }
            GeoIpMode::Whitelist => {
                if !self.config.allowed_countries.contains(&iso_code.to_string()) {
                    return Err(self.config.rejection_message.clone());
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
    fn test_geoip_disabled() {
        let config = GeoIpConfig {
            enabled: false,
            database_path: PathBuf::from("nonexistent.mmdb"),
            mode: GeoIpMode::Blacklist,
            blocked_countries: vec!["CN".to_string()],
            allowed_countries: vec![],
            rejection_message: "Blocked".to_string(),
        };

        let filter = GeoIpFilter::new(config).unwrap();
        let ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert!(filter.check_ip(ip).is_ok());
    }
}
