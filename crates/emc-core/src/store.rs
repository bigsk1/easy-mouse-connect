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
    #[serde(default)]
    pub share_clipboard_images: bool,
    pub share_files: bool,
    pub edge_switching: bool,
    pub peer_on_left: bool,
    #[serde(default)]
    pub keep_peer_awake: bool,
    #[serde(default)]
    pub show_status_notifications: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            share_clipboard: false,
            share_clipboard_images: false,
            share_files: false,
            edge_switching: true,
            peer_on_left: false,
            keep_peer_awake: false,
            show_status_notifications: false,
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalSettings {
    enabled: bool,
    share_clipboard: bool,
    share_files: bool,
    edge_switching: bool,
    peer_on_left: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Preferences {
    share_clipboard_images: bool,
    keep_peer_awake: bool,
    show_status_notifications: bool,
}

#[derive(Deserialize)]
struct PreferenceExtension {
    #[serde(default)]
    preferences: Preferences,
}

#[derive(Serialize)]
struct StoredConfig<'a> {
    version: u32,
    device_name: &'a str,
    private_key: &'a [u8],
    public_key: &'a [u8; 32],
    peer: &'a Option<Peer>,
    initiator: bool,
    settings: OriginalSettings,
    preferences: Preferences,
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
            let mut config: Self = serde_json::from_slice(&plain)
                .context("Cannot read identity; refusing to silently regenerate keys")?;
            // Keep the original five nested settings fields readable by 0.3.7.
            // New preferences occupy an optional top-level extension, which
            // older version-1 readers ignore without replacing trust or keys.
            let extension: PreferenceExtension = serde_json::from_slice(&plain)?;
            config.settings.share_clipboard_images = extension.preferences.share_clipboard_images;
            config.settings.keep_peer_awake = extension.preferences.keep_peer_awake;
            config.settings.show_status_notifications =
                extension.preferences.show_status_notifications;
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
        let plain = Zeroizing::new(serde_json::to_vec(&StoredConfig {
            version: self.version,
            device_name: &self.device_name,
            private_key: &self.private_key,
            public_key: &self.public_key,
            peer: &self.peer,
            initiator: self.initiator,
            settings: OriginalSettings {
                enabled: self.settings.enabled,
                share_clipboard: self.settings.share_clipboard,
                share_files: self.settings.share_files,
                edge_switching: self.settings.edge_switching,
                peer_on_left: self.settings.peer_on_left,
            },
            preferences: Preferences {
                share_clipboard_images: self.settings.share_clipboard_images,
                keep_peer_awake: self.settings.keep_peer_awake,
                show_status_notifications: self.settings.show_status_notifications,
            },
        })?);
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

    #[test]
    fn old_settings_default_new_permissions_off() {
        let original = r#"{"enabled":true,"share_clipboard":true,"share_files":false,"edge_switching":true,"peer_on_left":false}"#;
        let settings: Settings = serde_json::from_str(original).unwrap();
        assert!(settings.share_clipboard);
        assert!(!settings.share_clipboard_images);
        assert!(!settings.keep_peer_awake);
        assert!(!settings.show_status_notifications);
    }

    #[test]
    fn preferences_roundtrip_without_breaking_original_v1_readers() {
        let root = tempfile::tempdir().unwrap();
        let mut original = Config::load(root.path()).unwrap();
        original.initiator = true;
        original.peer = Some(Peer {
            name: "synthetic migration peer".into(),
            public_key: encode_key(&[9; 32]),
            address: "127.0.0.1:24859".into(),
        });
        original.settings.share_clipboard_images = true;
        original.settings.keep_peer_awake = true;
        original.settings.show_status_notifications = true;
        original.save(root.path()).unwrap();
        let current = Config::load(root.path()).unwrap();
        assert_eq!(current.public_key, original.public_key);
        assert_eq!(current.private_key, original.private_key);
        assert!(current.initiator);
        assert_eq!(
            current.peer.as_ref().unwrap().public_key,
            original.peer.as_ref().unwrap().public_key
        );
        assert!(current.settings.share_clipboard_images);
        assert!(current.settings.keep_peer_awake);
        assert!(current.settings.show_status_notifications);

        // A model of the old strict nested settings reader still accepts the
        // updated file. The extension is outside its known settings object.
        #[derive(Deserialize)]
        struct OriginalReader {
            version: u32,
            public_key: [u8; 32],
            initiator: bool,
            peer: Option<Peer>,
            settings: OriginalSettings,
        }
        let protected = fs::read(root.path().join("identity.dat")).unwrap();
        let plain = Zeroizing::new(emc_platform::unprotect_secret(&protected).unwrap());
        let old: OriginalReader = serde_json::from_slice(&plain).unwrap();
        assert_eq!(old.version, 1);
        assert_eq!(old.public_key, original.public_key);
        assert!(old.initiator);
        assert_eq!(
            old.peer.as_ref().unwrap().public_key,
            original.peer.as_ref().unwrap().public_key
        );
        assert!(old.settings.enabled);
    }
}
