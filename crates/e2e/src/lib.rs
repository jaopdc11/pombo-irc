//! End-to-end encryption for group rooms.
//!
//! Every member runs a Noise NN handshake with the room owner; that pairwise [`Session`] only
//! carries room keys. Room messages are sealed with the shared [`RoomKey`] by a [`RoomCipher`]
//! (XChaCha20-Poly1305, random nonce, length-hiding padding). [`Envelope`] tags what a relayed
//! payload holds. The fingerprint lets a member and the owner compare their session out of band.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use proto::{MAX_LINE, PAD_BLOCK};
use zeroize::Zeroizing;

/// Noise protocol name used by both sides.
const NOISE_PARAMS: &str = "Noise_NN_25519_ChaChaPoly_BLAKE2s";
/// ChaChaPoly authentication tag length.
const TAG_LEN: usize = 16;
/// Largest Noise message.
const MAX_NOISE_MSG: usize = 65535;
/// Size of the length prefix in a padded buffer.
const LEN_PREFIX: usize = 2;
/// Bytes of the handshake hash shown in the fingerprint.
const FINGERPRINT_BYTES: usize = 8;

/// Room key length.
const KEY_LEN: usize = 32;
/// Epoch prefix length on the wire.
const EPOCH_LEN: usize = 4;
/// XChaCha20 nonce length.
const NONCE_LEN: usize = 24;
/// Largest padded room plaintext: a tagged `MAX_LINE` line.
const MAX_ROOM_PADDED: usize = (LEN_PREFIX + 1 + MAX_LINE).div_ceil(PAD_BLOCK) * PAD_BLOCK;

const TAG_LINE: u8 = 0;
const TAG_TYPING: u8 = 1;

const ENV_HANDSHAKE: u8 = 0;
const ENV_KEY: u8 = 1;
const ENV_ROOM: u8 = 2;

#[derive(Debug, thiserror::Error)]
pub enum E2eError {
    #[error("noise: {0}")]
    Noise(#[from] snow::Error),
    #[error("line too long")]
    TooLong,
    #[error("bad padding")]
    BadPadding,
    #[error("malformed message")]
    BadMessage,
    #[error("unknown key epoch")]
    UnknownEpoch,
    #[error("replayed message")]
    Replay,
}

/// Which side of the handshake this is. The member is the initiator, the owner the responder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeRole {
    Initiator,
    Responder,
}

/// One side of the two-message NN handshake. The member is the initiator.
///
/// Order: initiator `write_message` → responder `read_message`, responder `write_message` →
/// initiator `read_message`; both are finished after that.
pub struct Handshake {
    state: snow::HandshakeState,
}

impl Handshake {
    /// Starts a handshake with fresh ephemeral keys.
    ///
    /// # Panics
    ///
    /// Only if the fixed Noise parameters fail to parse or the system RNG is unavailable.
    #[must_use]
    pub fn new(role: HandshakeRole) -> Handshake {
        let params = NOISE_PARAMS.parse().expect("valid noise params");
        let builder = snow::Builder::new(params);
        let state = match role {
            HandshakeRole::Initiator => builder.build_initiator(),
            HandshakeRole::Responder => builder.build_responder(),
        }
        .expect("noise handshake state");
        Handshake { state }
    }

    /// Produces the next handshake message to send to the peer.
    pub fn write_message(&mut self) -> Result<Vec<u8>, E2eError> {
        let mut buf = vec![0u8; MAX_NOISE_MSG];
        let n = self.state.write_message(&[], &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Consumes a handshake message from the peer.
    pub fn read_message(&mut self, msg: &[u8]) -> Result<(), E2eError> {
        let mut payload = Zeroizing::new(vec![0u8; MAX_NOISE_MSG]);
        let n = self.state.read_message(msg, &mut payload)?;
        if n != 0 {
            return Err(E2eError::BadMessage);
        }
        Ok(())
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// Switches to transport mode. Fails if the handshake is not finished.
    pub fn into_session(self) -> Result<Session, E2eError> {
        if !self.is_finished() {
            return Err(E2eError::Noise(snow::Error::State(
                snow::error::StateProblem::HandshakeNotFinished,
            )));
        }
        let fingerprint = fingerprint(self.state.get_handshake_hash());
        let transport = self.state.into_transport_mode()?;
        Ok(Session {
            transport,
            fingerprint,
        })
    }
}

/// An established pairwise session between a member and the owner. It only carries room keys.
pub struct Session {
    transport: snow::TransportState,
    fingerprint: String,
}

impl Session {
    /// The session fingerprint, `XXXX-XXXX-XXXX-XXXX`, identical on both sides.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Encrypts `key` (epoch BE ‖ 32 key bytes, padded) for the peer.
    pub fn seal_key(&mut self, key: &RoomKey) -> Result<Vec<u8>, E2eError> {
        let mut plain = Zeroizing::new([0u8; EPOCH_LEN + KEY_LEN]);
        plain[..EPOCH_LEN].copy_from_slice(&key.epoch.to_be_bytes());
        plain[EPOCH_LEN..].copy_from_slice(&key.key[..]);
        let padded = pad(&plain[..]);
        let mut out = vec![0u8; padded.len() + TAG_LEN];
        let n = self.transport.write_message(&padded, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    /// Decrypts a room key sealed by the peer with [`Session::seal_key`].
    pub fn open_key(&mut self, ct: &[u8]) -> Result<RoomKey, E2eError> {
        if ct.len() < TAG_LEN || ct.len() > MAX_NOISE_MSG {
            return Err(E2eError::BadMessage);
        }
        let mut padded = Zeroizing::new(vec![0u8; ct.len()]);
        let n = self.transport.read_message(ct, &mut padded)?;
        let plain = unpad(&padded[..n])?;
        if plain.len() != EPOCH_LEN + KEY_LEN {
            return Err(E2eError::BadMessage);
        }
        let (epoch, key_bytes) = plain.split_at(EPOCH_LEN);
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(key_bytes);
        Ok(RoomKey {
            epoch: u32::from_be_bytes([epoch[0], epoch[1], epoch[2], epoch[3]]),
            key,
        })
    }
}

/// A symmetric room key with its epoch. Its `Debug` output never shows the key.
#[derive(Clone)]
pub struct RoomKey {
    epoch: u32,
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl RoomKey {
    /// A fresh random key from the OS RNG.
    #[must_use]
    pub fn generate(epoch: u32) -> RoomKey {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        OsRng.fill_bytes(&mut key[..]);
        RoomKey { epoch, key }
    }

    #[must_use]
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&*self.key).into())
    }

    /// Associated data: epoch BE ‖ sender nick bytes.
    fn aad(&self, sender: &str) -> Vec<u8> {
        let mut aad = Vec::with_capacity(EPOCH_LEN + sender.len());
        aad.extend_from_slice(&self.epoch.to_be_bytes());
        aad.extend_from_slice(sender.as_bytes());
        aad
    }
}

impl std::fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomKey")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// What travels inside a room message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomMsg {
    Line(String),
    Typing,
}

/// Nonces already opened under one epoch.
type Seen = HashSet<[u8; NONCE_LEN]>;

/// Replay state, mutated by [`RoomCipher::open`].
struct Opened {
    /// Nonces opened under the current epoch.
    current: Seen,
    /// The previous key and its opened nonces, until the first current-epoch message opens.
    previous: Option<(RoomKey, Seen)>,
}

/// Seals and opens room messages with the current key; the previous epoch still opens until
/// the first message under the current epoch arrives. A nonce opens at most once per epoch.
///
/// Wire format: `epoch u32 BE ‖ nonce 24 ‖ XChaCha20-Poly1305(pad(tag ‖ utf8))`,
/// AAD = `epoch BE ‖ sender bytes`.
pub struct RoomCipher {
    current: RoomKey,
    opened: Mutex<Opened>,
}

impl RoomCipher {
    #[must_use]
    pub fn new(key: RoomKey) -> Self {
        RoomCipher {
            current: key,
            opened: Mutex::new(Opened {
                current: Seen::new(),
                previous: None,
            }),
        }
    }

    /// Installs `key` as current; the old current becomes previous and anything older is dropped.
    pub fn rotate(&mut self, key: RoomKey) {
        let old = std::mem::replace(&mut self.current, key);
        let opened = self
            .opened
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let seen = std::mem::take(&mut opened.current);
        opened.previous = Some((old, seen));
    }

    /// Epoch of the current key.
    #[must_use]
    pub fn epoch(&self) -> u32 {
        self.current.epoch
    }

    /// Encodes, pads and encrypts `msg` as sent by `sender`. A `Line` over `MAX_LINE` is refused.
    pub fn seal(&self, sender: &str, msg: &RoomMsg) -> Result<Vec<u8>, E2eError> {
        let mut plain = Zeroizing::new(Vec::new());
        match msg {
            RoomMsg::Line(text) => {
                if text.len() > MAX_LINE {
                    return Err(E2eError::TooLong);
                }
                plain.reserve_exact(1 + text.len());
                plain.push(TAG_LINE);
                plain.extend_from_slice(text.as_bytes());
            }
            RoomMsg::Typing => plain.push(TAG_TYPING),
        }
        let padded = pad(&plain);
        let key = &self.current;
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let aad = key.aad(sender);
        let ct = key
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: &padded,
                    aad: &aad,
                },
            )
            .map_err(|_| E2eError::BadMessage)?;
        let mut out = Vec::with_capacity(EPOCH_LEN + NONCE_LEN + ct.len());
        out.extend_from_slice(&key.epoch.to_be_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypts and decodes a room message, checking it was sealed by `sender`.
    pub fn open(&self, sender: &str, bytes: &[u8]) -> Result<RoomMsg, E2eError> {
        let header = EPOCH_LEN + NONCE_LEN;
        if bytes.len() < header + PAD_BLOCK + TAG_LEN
            || bytes.len() > header + MAX_ROOM_PADDED + TAG_LEN
        {
            return Err(E2eError::BadMessage);
        }
        let (epoch, rest) = bytes.split_at(EPOCH_LEN);
        let (nonce, ct) = rest.split_at(NONCE_LEN);
        let epoch = u32::from_be_bytes([epoch[0], epoch[1], epoch[2], epoch[3]]);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| E2eError::BadMessage)?;
        let mut opened = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
        let is_current = self.current.epoch == epoch;
        let (key, seen) = if is_current {
            (&self.current, &opened.current)
        } else {
            opened
                .previous
                .as_ref()
                .filter(|(k, _)| k.epoch == epoch)
                .map(|(k, s)| (k, s))
                .ok_or(E2eError::UnknownEpoch)?
        };
        if seen.contains(&nonce) {
            return Err(E2eError::Replay);
        }
        let aad = key.aad(sender);
        let padded = Zeroizing::new(
            key.cipher()
                .decrypt(XNonce::from_slice(&nonce), Payload { msg: ct, aad: &aad })
                .map_err(|_| E2eError::BadMessage)?,
        );
        if is_current {
            opened.current.insert(nonce);
            opened.previous = None;
        } else if let Some((_, seen)) = opened.previous.as_mut() {
            seen.insert(nonce);
        }
        drop(opened);
        let plain = unpad(&padded)?;
        match plain.split_first() {
            Some((&TAG_LINE, text)) => {
                if text.len() > MAX_LINE {
                    return Err(E2eError::TooLong);
                }
                let text = std::str::from_utf8(text).map_err(|_| E2eError::BadMessage)?;
                Ok(RoomMsg::Line(text.to_owned()))
            }
            Some((&TAG_TYPING, [])) => Ok(RoomMsg::Typing),
            _ => Err(E2eError::BadMessage),
        }
    }
}

/// What a relayed payload holds. Wire: one tag byte (0 handshake, 1 key, 2 room), then the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Envelope {
    Handshake(Vec<u8>),
    Key(Vec<u8>),
    Room(Vec<u8>),
}

impl Envelope {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let (tag, body) = match self {
            Envelope::Handshake(b) => (ENV_HANDSHAKE, b),
            Envelope::Key(b) => (ENV_KEY, b),
            Envelope::Room(b) => (ENV_ROOM, b),
        };
        let mut out = Vec::with_capacity(1 + body.len());
        out.push(tag);
        out.extend_from_slice(body);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Envelope, E2eError> {
        match bytes.split_first() {
            Some((&ENV_HANDSHAKE, body)) => Ok(Envelope::Handshake(body.to_vec())),
            Some((&ENV_KEY, body)) => Ok(Envelope::Key(body.to_vec())),
            Some((&ENV_ROOM, body)) => Ok(Envelope::Room(body.to_vec())),
            _ => Err(E2eError::BadMessage),
        }
    }
}

/// Formats the first 8 bytes of a handshake hash as `XXXX-XXXX-XXXX-XXXX` (uppercase hex).
#[must_use]
pub fn fingerprint(handshake_hash: &[u8]) -> String {
    let bytes = &handshake_hash[..handshake_hash.len().min(FINGERPRINT_BYTES)];
    bytes
        .chunks(2)
        .map(|g| g.iter().map(|b| format!("{b:02X}")).collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// Pads to the next multiple of `PAD_BLOCK`: `u16` big-endian length, payload, zero fill.
///
/// # Panics
///
/// If `plain` is longer than `u16::MAX` bytes.
#[must_use]
pub fn pad(plain: &[u8]) -> Zeroizing<Vec<u8>> {
    let len = u16::try_from(plain.len()).expect("padded payload fits in u16");
    let total = (LEN_PREFIX + plain.len()).div_ceil(PAD_BLOCK) * PAD_BLOCK;
    let mut out = Zeroizing::new(Vec::with_capacity(total));
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(plain);
    out.resize(total, 0);
    out
}

/// Reverses [`pad`], returning the payload slice.
pub fn unpad(padded: &[u8]) -> Result<&[u8], E2eError> {
    if padded.is_empty() || !padded.len().is_multiple_of(PAD_BLOCK) {
        return Err(E2eError::BadPadding);
    }
    let len = usize::from(u16::from_be_bytes([padded[0], padded[1]]));
    padded
        .get(LEN_PREFIX..LEN_PREFIX + len)
        .ok_or(E2eError::BadPadding)
}

#[cfg(test)]
mod tests;
