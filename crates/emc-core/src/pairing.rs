//! Short, random pairing credentials and the SPAKE2 bootstrap.
//!
//! The code is a PAKE password, never a Noise key or a hash lookup token. Each
//! fresh A/B exchange derives a session-specific 256-bit PSK with HKDF. Noise
//! subsequently confirms possession and authenticates the exchanged identities.
//! Upstream RustCrypto spake2 0.4 has not had an independent security audit.
use anyhow::{ensure, Context, Result};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use zeroize::Zeroizing;

pub(crate) const CODE_LENGTH: usize = 12;
pub(crate) const MAX_ATTEMPTS: u8 = 5;
pub(crate) const LIFETIME: Duration = Duration::from_secs(300);
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const MESSAGE_LENGTH: usize = 33;
const DOMAIN: &[u8] = b"EasyMouseConnect/Pairing/v2/SPAKE2-Ed25519/HKDF-SHA256";
const ID_A: &[u8] = b"EasyMouseConnect/EMC01/channel3/pairing-v2/initiator";
const ID_B: &[u8] = b"EasyMouseConnect/EMC01/channel3/pairing-v2/responder";
const PSK_INFO: &[u8] = b"EasyMouseConnect/Pairing/v2/Noise-XXpsk3/PSK";

#[derive(Clone)]
pub(crate) struct ManualCode(Zeroizing<[u8; CODE_LENGTH]>);

impl ManualCode {
    pub(crate) fn generate() -> Self {
        let mut random = Zeroizing::new([0; CODE_LENGTH]);
        OsRng.fill_bytes(&mut random[..]);
        for value in &mut random[..] {
            // 256 is divisible by 32: masking introduces no sampling bias.
            *value = ALPHABET[usize::from(*value & 31)];
        }
        Self(random)
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        ensure!(
            text.len() <= 32,
            "Enter the complete 12-character pairing code"
        );
        let mut code = Zeroizing::new([0; CODE_LENGTH]);
        let mut count = 0;
        for byte in text.bytes() {
            if byte == b'-' || byte.is_ascii_whitespace() {
                continue;
            }
            ensure!(
                count < CODE_LENGTH,
                "Enter the complete 12-character pairing code"
            );
            let byte = match byte.to_ascii_uppercase() {
                b'O' => b'0',
                b'I' | b'L' => b'1',
                byte => byte,
            };
            ensure!(
                ALPHABET.contains(&byte),
                "The pairing code contains an invalid character"
            );
            code[count] = byte;
            count += 1;
        }
        ensure!(
            count == CODE_LENGTH,
            "Enter the complete 12-character pairing code"
        );
        Ok(Self(code))
    }

    pub(crate) fn display(&self) -> String {
        let mut output = String::with_capacity(14);
        for (index, byte) in self.0.iter().enumerate() {
            if index == 4 || index == 8 {
                output.push('-');
            }
            output.push(char::from(*byte));
        }
        output
    }
}

/// Conservatively exclude code-shaped clipboard text as well as legacy tokens.
/// This deliberately also excludes unrelated text that happens to be 12 base32
/// characters; sharing less content is preferable to leaking a pairing secret.
pub(crate) fn looks_like_credential(text: &str) -> bool {
    text.trim().starts_with("emc1.") || ManualCode::parse(text.trim()).is_ok()
}

pub(crate) struct Derived {
    pub(crate) psk: Zeroizing<[u8; 32]>,
    pub(crate) transcript: [u8; 32],
}

struct Exchange {
    state: Spake2<Ed25519Group>,
    outbound: [u8; MESSAGE_LENGTH],
    initiator: bool,
}

impl Exchange {
    fn start(code: ManualCode, initiator: bool) -> Self {
        let password = Password::new(&code.0[..]);
        let a = Identity::new(ID_A);
        let b = Identity::new(ID_B);
        let (state, outbound) = if initiator {
            Spake2::<Ed25519Group>::start_a(&password, &a, &b)
        } else {
            Spake2::<Ed25519Group>::start_b(&password, &a, &b)
        };
        // RustCrypto's Ed25519Group always emits its role byte plus 32 bytes.
        let outbound = outbound.try_into().expect("SPAKE2 Ed25519 message size");
        Self {
            state,
            outbound,
            initiator,
        }
    }

    fn finish(self, inbound: &[u8; MESSAGE_LENGTH], header: &[u8]) -> Result<Derived> {
        let shared = Zeroizing::new(
            self.state
                .finish(inbound)
                .map_err(|_| anyhow::anyhow!("Invalid pairing exchange"))?,
        );
        let (a, b) = if self.initiator {
            (&self.outbound, inbound)
        } else {
            (inbound, &self.outbound)
        };
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(header);
        hash.update(ID_A);
        hash.update(ID_B);
        hash.update(a);
        hash.update(b);
        let transcript: [u8; 32] = hash.finalize().into();
        let mut psk = Zeroizing::new([0; 32]);
        Hkdf::<Sha256>::new(Some(&transcript), &shared)
            .expand(PSK_INFO, &mut psk[..])
            .map_err(|_| anyhow::anyhow!("Pairing key derivation failed"))?;
        Ok(Derived { psk, transcript })
    }
}

/// The initiator writes the preamble; the responder has already consumed it.
/// Fixed 33-byte PAKE records cannot trigger a peer-selected allocation.
pub(crate) async fn exchange(
    io: &mut TcpStream,
    code: &ManualCode,
    header: &[u8],
    initiator: bool,
) -> Result<Derived> {
    let code = code.clone();
    let header = header.to_vec();
    timeout(Duration::from_secs(10), async {
        if initiator {
            io.write_all(&header).await?;
        }
        let exchange =
            tokio::task::spawn_blocking(move || Exchange::start(code, initiator)).await?;
        let mut inbound = [0; MESSAGE_LENGTH];
        if initiator {
            io.write_all(&exchange.outbound).await?;
            io.read_exact(&mut inbound).await?;
        } else {
            io.read_exact(&mut inbound).await?;
            io.write_all(&exchange.outbound).await?;
        }
        tokio::task::spawn_blocking(move || exchange.finish(&inbound, &header)).await?
    })
    .await
    .context("Pairing exchange timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;
    const HEADER: &[u8] = b"EMC01\x03";

    #[test]
    fn code_roundtrip_is_twelve_unbiased_base32_characters() {
        for _ in 0..128 {
            let code = ManualCode::generate();
            let display = code.display();
            assert_eq!(display.len(), 14);
            assert_eq!(display.as_bytes()[4], b'-');
            assert_eq!(display.as_bytes()[9], b'-');
            assert_eq!(
                ManualCode::parse(&display.to_ascii_lowercase())
                    .unwrap()
                    .display(),
                display
            );
        }
        assert_eq!(
            ManualCode::parse("oIl1-2345-6789").unwrap().display(),
            "0111-2345-6789"
        );
        for invalid in [
            "",
            "123",
            "AAAA-AAAA-AAA",
            "AAAA-AAAA-AAAAA",
            "AAAA-AAAA-AAAU",
            "éAAA-AAAA-AAAA",
            "emc2.code",
            "AAAA_AAAA_AAAA",
        ] {
            assert!(ManualCode::parse(invalid).is_err());
        }
    }

    fn exchange_keys(password_a: &str, password_b: &str, header_b: &[u8]) -> (Derived, Derived) {
        let a = Exchange::start(ManualCode::parse(password_a).unwrap(), true);
        let b = Exchange::start(ManualCode::parse(password_b).unwrap(), false);
        let a_message = a.outbound;
        let b_message = b.outbound;
        (
            a.finish(&b_message, HEADER).unwrap(),
            b.finish(&a_message, header_b).unwrap(),
        )
    }

    #[test]
    fn fresh_exchange_matches_but_wrong_code_and_domain_do_not() {
        let (a, b) = exchange_keys("ABCD-EFGH-JKMP", "ABCD-EFGH-JKMP", HEADER);
        assert_eq!(*a.psk, *b.psk);
        assert_eq!(a.transcript, b.transcript);
        let (next, _) = exchange_keys("ABCD-EFGH-JKMP", "ABCD-EFGH-JKMP", HEADER);
        assert_ne!(*a.psk, *next.psk);
        let (a, b) = exchange_keys("ABCD-EFGH-JKMP", "ABCD-EFGH-JKMQ", HEADER);
        assert_ne!(*a.psk, *b.psk);
        let (a, b) = exchange_keys("ABCD-EFGH-JKMP", "ABCD-EFGH-JKMP", b"EMC01\x02");
        assert_ne!(*a.psk, *b.psk);
    }

    #[test]
    fn same_role_and_corrupt_messages_are_rejected() {
        let code = ManualCode::parse("ABCD-EFGH-JKMP").unwrap();
        let a = Exchange::start(code.clone(), true);
        let other_a = Exchange::start(code.clone(), true);
        assert!(a.finish(&other_a.outbound, HEADER).is_err());
        let a = Exchange::start(code, true);
        assert!(a.finish(&[0; MESSAGE_LENGTH], HEADER).is_err());
        assert!(looks_like_credential("ABCD-EFGH-JKMP"));
        assert!(!looks_like_credential("ordinary clipboard text"));
    }
}
