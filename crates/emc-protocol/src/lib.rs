//! Bounded, versioned wire messages. Never deserialize an unbounded network frame.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub const PORT: u16 = 24859;
pub const MAX_FILE_BYTES: u64 = 100_000_000;
pub const MAX_CLIPBOARD_BYTES: usize = 1_048_576;
pub const CHUNK_BYTES: usize = 48 * 1024;
pub const MAX_PLAINTEXT: usize = 64 * 1024 - 17;
pub const MAGIC: &[u8; 5] = b"EMC01";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    Move {
        dx: i32,
        dy: i32,
    },
    Button {
        button: MouseButton,
        down: bool,
    },
    Wheel {
        delta: i32,
        horizontal: bool,
    },
    Key {
        scan: u16,
        vk: u16,
        extended: bool,
        down: bool,
    },
    Enter {
        from_left: bool,
        y: u16,
    },
    Leave,
    ReleaseAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BulkKind {
    Text,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    Hello {
        name: String,
    },
    Ready,
    Input(InputEvent),
    Ping(u64),
    Pong(u64),
    Offer {
        id: String,
        name: String,
        size: u64,
        kind: BulkKind,
    },
    Decision {
        id: String,
        accept: bool,
    },
    Received {
        id: String,
    },
    Chunk {
        id: String,
        data: Vec<u8>,
    },
    Complete {
        id: String,
        sha256: [u8; 32],
    },
    Cancel {
        id: String,
    },
}

impl Message {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = postcard::to_stdvec(self)?;
        ensure!(bytes.len() <= MAX_PLAINTEXT, "Message exceeds frame limit");
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_PLAINTEXT, "Message exceeds frame limit");
        let (message, rest) = postcard::take_from_bytes(bytes)?;
        ensure!(rest.is_empty(), "Trailing bytes in message");
        Ok(message)
    }
}

/// Windows-safe basename; the receiver never accepts directories from its peer.
pub fn validate_filename(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 180,
        "Invalid filename length"
    );
    ensure!(
        !name.ends_with(['.', ' ']),
        "Trailing dot or space in filename"
    );
    ensure!(
        !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)),
        "Unsafe filename"
    );
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    ensure!(
        !["", "CON", "PRN", "AUX", "NUL", "CLOCK$"].contains(&stem.as_str()),
        "Reserved filename"
    );
    ensure!(
        !(stem.starts_with("COM") || stem.starts_with("LPT"))
            || !matches!(stem.chars().nth(3), Some('1'..='9' | '¹' | '²' | '³'))
            || stem.chars().count() != 4,
        "Reserved filename"
    );
    Ok(())
}

pub fn validate_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 32 && id.bytes().all(|x| x.is_ascii_hexdigit()),
        "Invalid transfer ID"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disallows_path_traversal_and_windows_devices() {
        for name in [
            "../a", "..", "C:\\x", "x:stream", "CON.txt", "COM1", "LPT9.zip", "NUL", "x.", "x ",
            "a/b", "a\nexe",
        ] {
            assert!(validate_filename(name).is_err(), "{name}");
        }
        for name in ["photo.png", "Résumé.txt", "archive.tar.gz", "COM10.txt"] {
            assert!(validate_filename(name).is_ok(), "{name}");
        }
    }
    #[test]
    fn bounded_roundtrip_and_rejects_trailing_data() {
        let message = Message::Input(InputEvent::Move { dx: -17, dy: 123 });
        let mut bytes = message.encode().unwrap();
        assert!(matches!(
            Message::decode(&bytes).unwrap(),
            Message::Input(InputEvent::Move { dx: -17, dy: 123 })
        ));
        bytes.push(0);
        assert!(Message::decode(&bytes).is_err());
        assert!(Message::Chunk {
            id: "f".repeat(32),
            data: vec![0; 65536]
        }
        .encode()
        .is_err());
    }
}
