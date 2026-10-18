use crate::transport;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub share_clipboard: bool,
    pub share_files: bool,
    pub edge_switching: bool,
    pub peer_on_left: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            share_clipboard: false,
            share_files: false,
            edge_switching: true,
            peer_on_left: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub name: String,
    pub public_key: String,
    pub address: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Config {
    pub version: u32,
    pub device_name: String,
    pub private_key: Vec<u8>,
    pub public_key: [u8; 32],
    pub peer: Option<Peer>,
    pub settings: Settings,
    pub initiator: bool,
}
impl Drop for Config {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.private_key.zeroize();
    }
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("identity.dat");
        if path.exists() {
            ensure!(
                fs::metadata(&path)?.len() < 65536,
                "Invalid configuration size"
            );
            let protected = fs::read(path)?;
            let plain = Zeroizing::new(emc_platform::unprotect_secret(&protected)?);
            let config: Self = serde_json::from_slice(&plain)
                .context("Cannot read identity; refusing to silently regenerate keys")?;
            ensure!(
                config.version == 1 && config.private_key.len() == 32,
                "Unsupported identity version"
            );
            if let Some(peer) = &config.peer {
                decode_key(&peer.public_key)?;
                parse_address(&peer.address)?;
            }
            return Ok(config);
        }
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        let key = transport::keypair()?;
        let config = Self {
            version: 1,
            device_name: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "This PC".into()),
            private_key: key.private,
            public_key: key
                .public
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid key"))?,
            peer: None,
            settings: Settings::default(),
            initiator: false,
        };
        config.save(root)?;
        Ok(config)
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        let plain = Zeroizing::new(serde_json::to_vec(self)?);
        let protected = emc_platform::protect_secret(&plain)?;
        let path = root.join("identity.tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(&protected)?;
        file.sync_all()?;
        drop(file);
        fs::rename(path, root.join("identity.dat"))?;
        Ok(())
    }
}
pub fn decode_key(key: &str) -> Result<[u8; 32]> {
    B64.decode(key)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid key length"))
}
pub fn encode_key(key: &[u8]) -> String {
    B64.encode(key)
}
pub fn parse_address(address: &str) -> Result<SocketAddr> {
    ensure!(address.len() <= 80, "Address too long");
    let addr = if let Ok(ip) = address.parse::<IpAddr>() {
        SocketAddr::new(ip, emc_protocol::PORT)
    } else {
        address
            .parse::<SocketAddr>()
            .context("Enter an IPv4 or IPv6 address, optionally with a port")?
    };
    ensure!(
        !addr.ip().is_unspecified() && !addr.ip().is_multicast() && addr.port() != 0,
        "Enter a unicast device address"
    );
    if let IpAddr::V4(ip) = addr.ip() {
        ensure!(
            ip.octets() != [255, 255, 255, 255],
            "Broadcast addresses are not supported"
        );
    }
    Ok(addr)
}
pub fn default_config_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("EasyMouseConnect")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
            .join("easy-mouse-connect")
    }
}
pub fn default_receive_dir() -> PathBuf {
    home().join("Downloads").join("EasyMouseConnect")
}
fn home() -> PathBuf {
    PathBuf::from(
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_routed_and_tailnet_ips_without_subnet_filters() {
        for ip in [
            "192.168.10.123",
            "192.168.50.123",
            "100.100.94.12",
            "100.71.9.14",
            "::1",
            "[fd7a:115c:a1e0::1]:24859",
        ] {
            assert!(parse_address(ip).is_ok(), "{ip}");
        }
        for ip in [
            "0.0.0.0",
            "255.255.255.255",
            "239.1.2.3",
            "[::]:1",
            "127.0.0.1:0",
            "example.com",
            "https://127.0.0.1",
        ] {
            assert!(parse_address(ip).is_err(), "{ip}");
        }
    }
    #[test]
    fn identity_persists_and_corruption_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let a = Config::load(root.path()).unwrap();
        let b = Config::load(root.path()).unwrap();
        assert_eq!(a.public_key, b.public_key);
        fs::write(root.path().join("identity.dat"), b"broken").unwrap();
        assert!(Config::load(root.path()).is_err());
    }
}
