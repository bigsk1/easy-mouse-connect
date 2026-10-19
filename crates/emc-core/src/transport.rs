//! Noise over bounded TCP frames. No application message precedes authentication.
use crate::pairing::{self, ManualCode};
use anyhow::{bail, ensure, Context, Result};
use emc_protocol::{Message, LEGACY_MAGIC, MAGIC};
use snow::{Builder, TransportState};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
    time::timeout,
};

pub const PAIR: u8 = 0;
pub const CONTROL: u8 = 1;
pub const BULK: u8 = 2;
/// SPAKE2 followed by transcript-bound Noise XXpsk3 on this same connection.
pub const PAIR_PAKE: u8 = 3;
const XX: &str = "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s";
const IK: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";

pub fn keypair() -> Result<snow::Keypair> {
    Ok(Builder::new(IK.parse()?).generate_keypair()?)
}

pub struct Channel {
    pub reader: Reader,
    pub writer: Writer,
    pub remote_key: [u8; 32],
}
pub struct Reader {
    io: OwnedReadHalf,
    noise: Arc<Mutex<TransportState>>,
}
pub struct Writer {
    io: OwnedWriteHalf,
    noise: Arc<Mutex<TransportState>>,
}

async fn read_frame(io: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>> {
    let len = io.read_u16().await? as usize;
    ensure!((16..=65535).contains(&len), "Invalid frame length");
    let mut bytes = vec![0; len];
    io.read_exact(&mut bytes).await?;
    Ok(bytes)
}
async fn write_frame(io: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    ensure!((16..=65535).contains(&bytes.len()), "Invalid frame length");
    let mut framed = Vec::with_capacity(bytes.len() + 2);
    framed.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    framed.extend_from_slice(bytes);
    io.write_all(&framed).await?;
    Ok(())
}

/// The preamble is bound into Noise's prologue, preventing channel/version substitution.
pub async fn preamble(io: &mut TcpStream) -> Result<u8> {
    let mut header = [0; 6];
    timeout(Duration::from_secs(5), io.read_exact(&mut header)).await??;
    ensure!(
        &header[..5] == MAGIC,
        "Unsupported protocol version; update both applications"
    );
    ensure!(
        header[5] <= PAIR_PAKE,
        "Unknown channel; update both applications"
    );
    Ok(header[5])
}

pub async fn connect(
    io: TcpStream,
    kind: u8,
    private: &[u8],
    expected: &[u8; 32],
    psk: Option<&[u8; 32]>,
) -> Result<Channel> {
    handshake(
        io,
        Handshake {
            kind,
            private,
            expected: Some(expected),
            psk,
            initiator: true,
            extension: &[],
            write_preamble: true,
        },
    )
    .await
}
pub async fn accept(
    io: TcpStream,
    kind: u8,
    private: &[u8],
    expected: Option<&[u8; 32]>,
    psk: Option<&[u8; 32]>,
) -> Result<Channel> {
    handshake(
        io,
        Handshake {
            kind,
            private,
            expected,
            psk,
            initiator: false,
            extension: &[],
            write_preamble: false,
        },
    )
    .await
}

pub(crate) async fn connect_with_code(
    mut io: TcpStream,
    private: &[u8],
    code: &ManualCode,
) -> Result<Channel> {
    io.set_nodelay(true)?;
    let header = pairing_header();
    let derived = pairing::exchange(&mut io, code, &header, true).await?;
    handshake(
        io,
        Handshake {
            kind: PAIR_PAKE,
            private,
            expected: None,
            psk: Some(&derived.psk),
            initiator: true,
            extension: &derived.transcript,
            write_preamble: false,
        },
    )
    .await
}

pub(crate) async fn accept_with_code(
    mut io: TcpStream,
    private: &[u8],
    code: &ManualCode,
) -> Result<Channel> {
    io.set_nodelay(true)?;
    let header = pairing_header();
    let derived = pairing::exchange(&mut io, code, &header, false).await?;
    handshake(
        io,
        Handshake {
            kind: PAIR_PAKE,
            private,
            expected: None,
            psk: Some(&derived.psk),
            initiator: false,
            extension: &derived.transcript,
            write_preamble: false,
        },
    )
    .await
}

fn pairing_header() -> Vec<u8> {
    let mut header = MAGIC.to_vec();
    header.push(PAIR_PAKE);
    header
}

struct Handshake<'a> {
    kind: u8,
    private: &'a [u8],
    expected: Option<&'a [u8; 32]>,
    psk: Option<&'a [u8; 32]>,
    initiator: bool,
    extension: &'a [u8],
    write_preamble: bool,
}

async fn handshake(mut io: TcpStream, options: Handshake<'_>) -> Result<Channel> {
    let Handshake {
        kind,
        private,
        expected,
        psk,
        initiator,
        extension,
        write_preamble,
    } = options;
    ensure!(kind <= PAIR_PAKE, "Unknown channel");
    ensure!(
        kind != PAIR_PAKE || extension.len() == 32,
        "Missing pairing transcript binding"
    );
    io.set_nodelay(true)?;
    let mut header = if kind == PAIR { LEGACY_MAGIC } else { MAGIC }.to_vec();
    header.push(kind);
    let pairing = kind == PAIR || kind == PAIR_PAKE;
    let mut prologue = header.clone();
    if kind == PAIR_PAKE {
        prologue.extend_from_slice(b"EasyMouseConnect/Pairing/v2/Noise-XXpsk3/transcript");
        prologue.extend_from_slice(extension);
    }
    let channel = timeout(Duration::from_secs(8), async {
        if write_preamble {
            io.write_all(&header).await?;
        }
        let mut builder = Builder::new(if pairing { XX } else { IK }.parse()?)
            .local_private_key(private)?
            .prologue(&prologue)?;
        if pairing {
            builder = builder.psk(3, psk.context("Pairing is closed")?)?;
        } else if initiator {
            builder = builder.remote_public_key(expected.context("Missing peer identity")?)?;
        }
        let mut noise = if initiator {
            builder.build_initiator()?
        } else {
            builder.build_responder()?
        };
        let mut out = vec![0; 65535];
        while !noise.is_handshake_finished() {
            if noise.is_my_turn() {
                let n = noise.write_message(&[], &mut out)?;
                write_frame(&mut io, &out[..n]).await?;
            } else {
                let bytes = read_frame(&mut io).await?;
                noise.read_message(&bytes, &mut out)?;
            }
        }
        let remote_key: [u8; 32] = noise
            .get_remote_static()
            .context("Missing remote identity")?
            .try_into()?;
        if let Some(expected) = expected {
            ensure!(
                &remote_key == expected,
                "Peer identity changed; reconnect refused"
            );
        } else if !pairing {
            bail!("Untrusted peer");
        }
        let noise = Arc::new(Mutex::new(noise.into_transport_mode()?));
        let (read, write) = io.into_split();
        Ok::<_, anyhow::Error>(Channel {
            reader: Reader {
                io: read,
                noise: noise.clone(),
            },
            writer: Writer { io: write, noise },
            remote_key,
        })
    })
    .await
    .context("Authentication timed out")??;
    Ok(channel)
}

impl Reader {
    pub async fn recv(&mut self) -> Result<Message> {
        let bytes = read_frame(&mut self.io).await?;
        let mut plain = vec![0; bytes.len() - 16];
        let n = self
            .noise
            .lock()
            .unwrap()
            .read_message(&bytes, &mut plain)?;
        Message::decode(&plain[..n])
    }
}
impl Writer {
    pub async fn send(&mut self, message: &Message) -> Result<()> {
        let plain = message.encode()?;
        let mut out = vec![0; plain.len() + 18];
        let n = self
            .noise
            .lock()
            .unwrap()
            .write_message(&plain, &mut out[2..])?;
        ensure!((16..=65535).contains(&n), "Invalid encrypted frame length");
        out[..2].copy_from_slice(&(n as u16).to_be_bytes());
        timeout(Duration::from_secs(5), self.io.write_all(&out[..n + 2]))
            .await
            .context("Peer stopped reading")??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn pake_then_noise_confirms_code_and_authenticates_both_static_keys() {
        let a = keypair().unwrap();
        let b = keypair().unwrap();
        let code = ManualCode::parse("ABCD-EFGH-JKMP").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (server, client) = tokio::join!(
            async {
                let (mut io, _) = listener.accept().await?;
                ensure!(preamble(&mut io).await? == PAIR_PAKE, "Wrong channel");
                accept_with_code(io, &b.private, &code).await
            },
            async {
                connect_with_code(TcpStream::connect(address).await?, &a.private, &code).await
            }
        );
        let mut server = server.unwrap();
        let mut client = client.unwrap();
        assert_eq!(&server.remote_key[..], &a.public);
        assert_eq!(&client.remote_key[..], &b.public);
        client
            .writer
            .send(&Message::Hello {
                name: "synthetic peer".into(),
            })
            .await
            .unwrap();
        assert!(matches!(
            server.reader.recv().await.unwrap(),
            Message::Hello { .. }
        ));
        server.writer.send(&Message::Ready).await.unwrap();
        assert!(matches!(
            client.reader.recv().await.unwrap(),
            Message::Ready
        ));
    }

    #[tokio::test]
    async fn short_codes_have_no_legacy_or_control_channel_fallback() {
        let a = keypair().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let io = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        // Directly using a code-derived/raw PSK without the transcript cannot
        // construct the PAKE channel, even through the legacy helper.
        assert!(connect(io, PAIR_PAKE, &a.private, &[0; 32], Some(&[0; 32]))
            .await
            .is_err());
    }
    async fn pair(
        psk_a: [u8; 32],
        psk_b: [u8; 32],
        wrong_pin: bool,
    ) -> (Result<Channel>, Result<Channel>) {
        let a = keypair().unwrap();
        let b = keypair().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pin = if wrong_pin {
            [0; 32]
        } else {
            b.public.clone().try_into().unwrap()
        };
        tokio::join!(
            async {
                let (mut io, _) = listener.accept().await?;
                // Emulate an old receiver only for the explicit outbound
                // legacy invitation import. The current listener rejects it.
                let mut header = [0; 6];
                io.read_exact(&mut header).await?;
                ensure!(
                    &header[..5] == LEGACY_MAGIC && header[5] == PAIR,
                    "Wrong legacy preamble"
                );
                accept(io, PAIR, &b.private, None, Some(&psk_b)).await
            },
            async {
                connect(
                    TcpStream::connect(addr).await?,
                    PAIR,
                    &a.private,
                    &pin,
                    Some(&psk_a),
                )
                .await
            }
        )
    }
    #[tokio::test]
    async fn pair_and_encrypted_bidirectional_messages() {
        let (a, b) = pair([7; 32], [7; 32], false).await;
        let mut a = a.unwrap();
        let mut b = b.unwrap();
        a.writer.send(&Message::Ping(42)).await.unwrap();
        assert!(matches!(b.reader.recv().await.unwrap(), Message::Ping(42)));
        b.writer.send(&Message::Pong(42)).await.unwrap();
        assert!(matches!(a.reader.recv().await.unwrap(), Message::Pong(42)));
    }
    #[tokio::test]
    async fn current_listener_explicitly_rejects_every_legacy_channel() {
        for kind in [PAIR, CONTROL, BULK, PAIR_PAKE] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (server, ()) = tokio::join!(
                async {
                    let (mut io, _) = listener.accept().await.unwrap();
                    preamble(&mut io).await
                },
                async {
                    let mut io = TcpStream::connect(addr).await.unwrap();
                    let mut header = LEGACY_MAGIC.to_vec();
                    header.push(kind);
                    io.write_all(&header).await.unwrap();
                }
            );
            assert!(server
                .unwrap_err()
                .to_string()
                .contains("update both applications"));
        }
    }
    #[tokio::test]
    async fn wrong_secret_or_changed_identity_fails_closed() {
        let (a, _) = pair([7; 32], [8; 32], false).await;
        assert!(a.is_err());
        let (_, b) = pair([7; 32], [7; 32], true).await;
        assert!(b.is_err());
    }
    #[tokio::test]
    async fn reconnect_requires_mutually_pinned_keys() {
        let a = keypair().unwrap();
        let b = keypair().unwrap();
        let intruder = keypair().unwrap();
        for trusted in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let expected: [u8; 32] = if trusted {
                a.public.clone()
            } else {
                intruder.public.clone()
            }
            .try_into()
            .unwrap();
            let pin = b.public.clone().try_into().unwrap();
            let (server, client) = tokio::join!(
                async {
                    let (mut io, _) = listener.accept().await?;
                    let kind = preamble(&mut io).await?;
                    accept(io, kind, &b.private, Some(&expected), None).await
                },
                async {
                    connect(
                        TcpStream::connect(addr).await?,
                        CONTROL,
                        &a.private,
                        &pin,
                        None,
                    )
                    .await
                }
            );
            assert!(client.is_ok());
            assert_eq!(server.is_ok(), trusted);
        }
    }
    #[test]
    fn tamper_replay_and_wrong_channel_prologue_rejected() {
        let a = keypair().unwrap();
        let b = keypair().unwrap();
        let mut x = Builder::new(IK.parse().unwrap())
            .local_private_key(&a.private)
            .unwrap()
            .remote_public_key(&b.public)
            .unwrap()
            .prologue(b"control")
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut wrong = Builder::new(IK.parse().unwrap())
            .local_private_key(&b.private)
            .unwrap()
            .prologue(b"bulk")
            .unwrap()
            .build_responder()
            .unwrap();
        let mut y = Builder::new(IK.parse().unwrap())
            .local_private_key(&b.private)
            .unwrap()
            .prologue(b"control")
            .unwrap()
            .build_responder()
            .unwrap();
        let mut buf = [0; 1024];
        let mut plain = [0; 1024];
        let n = x.write_message(&[], &mut buf).unwrap();
        assert!(wrong.read_message(&buf[..n], &mut plain).is_err());
        y.read_message(&buf[..n], &mut plain).unwrap();
        let n = y.write_message(&[], &mut buf).unwrap();
        x.read_message(&buf[..n], &mut plain).unwrap();
        let mut x = x.into_transport_mode().unwrap();
        let mut y = y.into_transport_mode().unwrap();
        let n = x.write_message(b"test", &mut buf).unwrap();
        let mut tampered = buf[..n].to_vec();
        tampered[0] ^= 1;
        assert!(y.read_message(&tampered, &mut plain).is_err());
        assert!(y.read_message(&buf[..n], &mut plain).is_ok());
        assert!(y.read_message(&buf[..n], &mut plain).is_err());
    }
}
