use super::*;

fn pair() -> (Session, Session) {
    let mut init = Handshake::new(HandshakeRole::Initiator);
    let mut resp = Handshake::new(HandshakeRole::Responder);
    let m1 = init.write_message().unwrap();
    resp.read_message(&m1).unwrap();
    let m2 = resp.write_message().unwrap();
    init.read_message(&m2).unwrap();
    assert!(init.is_finished());
    assert!(resp.is_finished());
    (init.into_session().unwrap(), resp.into_session().unwrap())
}

fn regex_like(fp: &str) -> bool {
    fp.len() == 19
        && fp.split('-').count() == 4
        && fp.split('-').all(|g| {
            g.len() == 4
                && g.chars()
                    .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c))
        })
}

#[test]
fn both_sides_agree_on_fingerprint() {
    let (a, b) = pair();
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert!(regex_like(a.fingerprint()));
}

#[test]
fn fresh_keys_every_conversation() {
    let (a, _) = pair();
    let (c, _) = pair();
    assert_ne!(a.fingerprint(), c.fingerprint());
}

#[test]
fn handshake_out_of_order_is_error() {
    let mut resp = Handshake::new(HandshakeRole::Responder);
    assert!(resp.write_message().is_err());
    let init = Handshake::new(HandshakeRole::Initiator);
    assert!(!init.is_finished());
    assert!(init.into_session().is_err());
}

#[test]
fn fingerprint_format() {
    assert_eq!(
        fingerprint(&[0x7f, 0x3a, 0x91, 0xc2, 0x0b, 0x5e, 0xd4, 0xa8, 0xff]),
        "7F3A-91C2-0B5E-D4A8"
    );
}

#[test]
fn pad_unpad() {
    assert_eq!(pad(b"").len(), 256);
    assert_eq!(pad(&[1; 254]).len(), 256);
    assert_eq!(pad(&[1; 255]).len(), 512);
    for x in [&b""[..], &[1; 254][..], &[7; 255][..], b"hello"] {
        assert_eq!(unpad(&pad(x)).unwrap(), x);
    }
    let mut bad = vec![0u8; 256];
    bad[0] = 0x01; // length 256 + payload overflows a 256-byte block
    assert!(unpad(&bad).is_err());
    assert!(unpad(&[]).is_err());
    assert!(unpad(&[0u8]).is_err());
}

fn cipher(epoch: u32) -> RoomCipher {
    RoomCipher::new(RoomKey::generate(epoch))
}

#[test]
fn key_update_over_noise_roundtrip() {
    let (mut member, mut owner) = pair();
    let key = RoomKey::generate(7);
    let owner_cipher = RoomCipher::new(key.clone());
    let sealed = owner.seal_key(&key).unwrap();
    assert!(!sealed.windows(32).any(|w| w == &key.key[..]));
    assert_eq!(sealed.len(), PAD_BLOCK + 16);
    let received = member.open_key(&sealed).unwrap();
    assert_eq!(received.epoch(), 7);
    let member_cipher = RoomCipher::new(received);
    let msg = RoomMsg::Line("chave chegou".into());
    let c = owner_cipher.seal("owner", &msg).unwrap();
    assert_eq!(member_cipher.open("owner", &c).unwrap(), msg);
    // The channel works in both directions and tampering is caught.
    let mut back = member.seal_key(&RoomKey::generate(8)).unwrap();
    assert_eq!(owner.open_key(&back.clone()).unwrap().epoch(), 8);
    back[3] ^= 1;
    assert!(owner.open_key(&back).is_err());
    assert!(member.open_key(&[0u8; 3]).is_err());
}

#[test]
fn room_key_debug_hides_key() {
    let key = RoomKey::generate(3);
    let s = format!("{key:?}");
    assert!(s.contains('3'));
    assert!(s.len() < 64);
}

#[test]
fn room_seal_open_roundtrip() {
    let c = cipher(1);
    for msg in [
        RoomMsg::Line("tá na rede do cliente? 🐦".into()),
        RoomMsg::Line(String::new()),
        RoomMsg::Line("x".repeat(MAX_LINE)),
        RoomMsg::Typing,
    ] {
        let bytes = c.seal("alice", &msg).unwrap();
        assert_eq!(c.open("alice", &bytes).unwrap(), msg);
    }
    assert_eq!(c.epoch(), 1);
}

#[test]
fn room_line_too_long() {
    let c = cipher(1);
    let r = c.seal("alice", &RoomMsg::Line("x".repeat(MAX_LINE + 1)));
    assert!(matches!(r, Err(E2eError::TooLong)));
}

#[test]
fn room_open_with_wrong_sender_fails() {
    let c = cipher(1);
    let bytes = c.seal("alice", &RoomMsg::Line("oi".into())).unwrap();
    assert!(c.open("mallory", &bytes).is_err());
    assert!(c.open("alic", &bytes).is_err());
}

#[test]
fn room_tampered_and_garbage_rejected() {
    let c = cipher(1);
    let mut bytes = c.seal("alice", &RoomMsg::Typing).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert!(c.open("alice", &bytes).is_err());
    assert!(c.open("alice", &[]).is_err());
    assert!(c.open("alice", &[0u8; 30]).is_err());
    // Another room key cannot open it.
    let bytes = c.seal("alice", &RoomMsg::Typing).unwrap();
    assert!(cipher(1).open("alice", &bytes).is_err());
}

#[test]
fn previous_epoch_still_opens() {
    let mut c = cipher(1);
    let old = c.seal("bob", &RoomMsg::Line("late".into())).unwrap();
    c.rotate(RoomKey::generate(2));
    assert_eq!(c.epoch(), 2);
    assert_eq!(c.open("bob", &old).unwrap(), RoomMsg::Line("late".into()));
    let new = c.seal("bob", &RoomMsg::Typing).unwrap();
    assert_eq!(&new[..4], &2u32.to_be_bytes());
    assert_eq!(c.open("bob", &new).unwrap(), RoomMsg::Typing);
}

#[test]
fn replayed_message_rejected() {
    let c = cipher(1);
    let bytes = c.seal("alice", &RoomMsg::Line("uma vez".into())).unwrap();
    assert_eq!(
        c.open("alice", &bytes).unwrap(),
        RoomMsg::Line("uma vez".into())
    );
    assert!(matches!(c.open("alice", &bytes), Err(E2eError::Replay)));
    // A fresh nonce still opens.
    let other = c.seal("alice", &RoomMsg::Typing).unwrap();
    assert_eq!(c.open("alice", &other).unwrap(), RoomMsg::Typing);
}

#[test]
fn replayed_previous_epoch_message_rejected() {
    let mut c = cipher(1);
    let old = c.seal("bob", &RoomMsg::Typing).unwrap();
    c.rotate(RoomKey::generate(2));
    assert_eq!(c.open("bob", &old).unwrap(), RoomMsg::Typing);
    assert!(matches!(c.open("bob", &old), Err(E2eError::Replay)));
}

#[test]
fn previous_epoch_dropped_after_current_opens() {
    let mut c = cipher(1);
    let old = c.seal("bob", &RoomMsg::Line("late".into())).unwrap();
    c.rotate(RoomKey::generate(2));
    let new = c.seal("bob", &RoomMsg::Typing).unwrap();
    assert_eq!(c.open("bob", &new).unwrap(), RoomMsg::Typing);
    assert!(matches!(c.open("bob", &old), Err(E2eError::UnknownEpoch)));
}

#[test]
fn two_epochs_back_fails() {
    let mut c = cipher(1);
    let old = c.seal("bob", &RoomMsg::Line("too late".into())).unwrap();
    c.rotate(RoomKey::generate(2));
    c.rotate(RoomKey::generate(3));
    assert!(c.open("bob", &old).is_err());
}

#[test]
fn room_ciphertext_constant_size() {
    let c = cipher(1);
    let short = c.seal("a", &RoomMsg::Line("hi".into())).unwrap();
    let longer = c.seal("a", &RoomMsg::Line("x".repeat(200))).unwrap();
    let typing = c.seal("a", &RoomMsg::Typing).unwrap();
    assert_eq!(short.len(), longer.len());
    assert_eq!(short.len(), typing.len());
    assert_eq!(short.len(), 4 + 24 + 256 + 16);
}

#[test]
fn room_ciphertext_has_no_plaintext() {
    let c = cipher(1);
    let bytes = c
        .seal("alice", &RoomMsg::Line("segredo-canario".into()))
        .unwrap();
    assert!(!bytes.windows(15).any(|w| w == b"segredo-canario"));
}

#[test]
fn nonces_differ() {
    let c = cipher(1);
    let msg = RoomMsg::Line("same".into());
    let a = c.seal("alice", &msg).unwrap();
    let b = c.seal("alice", &msg).unwrap();
    assert_ne!(a, b);
    assert_ne!(a[4..28], b[4..28]);
}

#[test]
fn envelope_roundtrip_and_garbage() {
    for env in [
        Envelope::Handshake(vec![1, 2, 3]),
        Envelope::Key(vec![]),
        Envelope::Room(vec![9; 300]),
    ] {
        assert_eq!(Envelope::decode(&env.encode()).unwrap(), env);
    }
    assert_eq!(Envelope::Key(vec![5]).encode(), vec![1, 5]);
    assert!(Envelope::decode(&[]).is_err());
    assert!(Envelope::decode(&[3, 1, 2]).is_err());
    assert!(Envelope::decode(&[255]).is_err());
}
