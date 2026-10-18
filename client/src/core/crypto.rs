//! The cryptography of private conversations, and nothing else: no networking, no files, no UI.
//!
//! - Each person has one X25519 identity key. Its private half is made here and leaves this
//!   computer only sealed under a recovery key the server never sees (`wrap`).
//! - Two people agree a key per conversation and per pair of identity keys (`pair_key`): X25519,
//!   then HKDF-SHA256 bound to the conversation, both people and both public keys, so a key for one
//!   conversation is useless in any other.
//! - Everything is sealed with XChaCha20-Poly1305 under a random 24-byte nonce, with what it is
//!   (a message, a reaction, a call key) and where it belongs bound in as associated data: the
//!   server can't move a sealed reaction onto another message or replay a message into another
//!   conversation without it failing to open.
//! - Files are sealed under a fresh random key each, which travels inside the sealed message.
//!
//! Sealed text is base64url of `version ‖ nonce ‖ ciphertext+tag`.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// The format of everything sealed here. Bumped if it ever changes, so old and new can be told apart.
const VERSION: u8 = 1;
const NONCE: usize = 24;
const TAG: usize = 16;

pub type UserId = i64;

/// Bytes from the operating system's generator. Without one there is no safe way on, so it panics
/// rather than seal anything with predictable bytes.
pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::getrandom(&mut out).expect("the system's random number generator failed");
    out
}

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn unb64(text: &str) -> Option<Vec<u8>> {
    B64.decode(text.trim()).ok()
}

/// A public key as the server carries it, or None if it is not 32 bytes of base64url.
pub fn public_key(text: &str) -> Option<[u8; 32]> {
    unb64(text)?.try_into().ok()
}

fn seal_with(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    seal_with_nonce(key, aad, plain, random())
}

/// `seal_with` under a nonce of the caller's: only the tests pin one, to check the format against
/// fixed bytes. Anything that leaves this computer gets a fresh random one.
fn seal_with_nonce(key: &[u8; 32], aad: &[u8], plain: &[u8], nonce: [u8; NONCE]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let sealed = cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad }).expect("XChaCha20-Poly1305 does not fail to seal");
    let mut out = Vec::with_capacity(1 + NONCE + sealed.len());
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    out
}

fn open_with(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 1 + NONCE + TAG || sealed[0] != VERSION {
        return None;
    }
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher.decrypt(XNonce::from_slice(&sealed[1..1 + NONCE]), Payload { msg: &sealed[1 + NONCE..], aad }).ok()
}

// Identity.

/// Your identity key. The private half is wiped from memory when this is dropped.
pub struct Identity {
    secret: StaticSecret,
    public: PublicKey,
}

impl Identity {
    pub fn generate() -> Identity {
        Identity::from_secret(Zeroizing::new(random()))
    }

    pub fn from_secret(bytes: Zeroizing<[u8; 32]>) -> Identity {
        let secret = StaticSecret::from(*bytes);
        let public = PublicKey::from(&secret);
        Identity { secret, public }
    }

    pub fn public(&self) -> [u8; 32] {
        self.public.to_bytes()
    }

    pub fn public_b64(&self) -> String {
        b64(self.public.as_bytes())
    }

    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.secret.to_bytes())
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.public_b64())
    }
}

// Recovery.

/// Crockford's base32: no I, L, O or U, so nothing in it is mistaken for something else when
/// copied off a screen or a piece of paper.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The recovery key: 160 random bits, shown as 32 characters in eight groups of four. It opens
/// your identity key on a computer that does not have it yet; nothing else can.
pub struct RecoveryKey(Zeroizing<[u8; 20]>);

impl RecoveryKey {
    pub fn generate() -> RecoveryKey {
        RecoveryKey(Zeroizing::new(random()))
    }

    /// Reads one back however it was typed: any case, with or without dashes and spaces, and
    /// with the letters people confuse (O for 0, I and L for 1) taken as what they meant.
    pub fn parse(text: &str) -> Option<RecoveryKey> {
        let mut bits: u64 = 0;
        let mut n = 0;
        let mut out = Zeroizing::new([0u8; 20]);
        let mut at = 0;
        for c in text.chars().filter(|c| !c.is_whitespace() && *c != '-') {
            let c = match c.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            let v = CROCKFORD.iter().position(|&x| x as char == c)? as u64;
            bits = (bits << 5) | v;
            n += 5;
            if n >= 8 {
                n -= 8;
                *out.get_mut(at)? = (bits >> n) as u8;
                at += 1;
                bits &= (1 << n) - 1;
            }
        }
        (at == 20 && n == 0).then_some(RecoveryKey(out))
    }

    pub fn display(&self) -> String {
        let mut chars = String::with_capacity(39);
        let (mut bits, mut n) = (0u64, 0);
        for &b in self.0.iter() {
            bits = (bits << 8) | b as u64;
            n += 8;
            while n >= 5 {
                n -= 5;
                if !chars.is_empty() && (chars.len() + 1).is_multiple_of(5) {
                    chars.push('-');
                }
                chars.push(CROCKFORD[((bits >> n) & 31) as usize] as char);
            }
            bits &= (1 << n) - 1;
        }
        chars
    }

    fn wrapping_key(&self) -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(Some(b"harmony-recovery-v1"), self.0.as_slice())
            .expand(b"wrap identity", key.as_mut_slice())
            .expect("32 bytes is a valid HKDF length");
        key
    }
}

/// Your identity key's private half, sealed under the recovery key: what the server keeps, and
/// all it ever gets of it. The public key is bound in, so a wrapping can't be swapped for another.
pub fn wrap(identity: &Identity, recovery: &RecoveryKey) -> String {
    b64(&seal_with(&recovery.wrapping_key(), &identity.public(), identity.secret_bytes().as_slice()))
}

/// The identity key back out of a wrapping, if this recovery key is the one it was sealed under.
pub fn unwrap(wrapped: &str, public: &[u8; 32], recovery: &RecoveryKey) -> Option<Identity> {
    let plain = Zeroizing::new(open_with(&recovery.wrapping_key(), public, &unb64(wrapped)?)?);
    let secret: [u8; 32] = plain.as_slice().try_into().ok()?;
    let identity = Identity::from_secret(Zeroizing::new(secret));
    (identity.public() == *public).then_some(identity)
}

// Conversations.

/// The key two people share for one conversation and one pair of their identity keys.
pub struct PairKey(Zeroizing<[u8; 32]>);

/// Agrees the key for a conversation. Symmetric: each side, with its own private key and the
/// other's public one, arrives at the same key. None when the other key is one X25519 must never
/// be used with (a small-order point, which would make the "shared" secret known to anybody).
pub fn pair_key(mine: &Identity, me: UserId, their_public: &[u8; 32], them: UserId, conversation: i64) -> Option<PairKey> {
    let shared = mine.secret.diffie_hellman(&PublicKey::from(*their_public));
    if !shared.was_contributory() {
        return None;
    }
    let (first, second) =
        if me < them { ((me, mine.public()), (them, *their_public)) } else { ((them, *their_public), (me, mine.public())) };
    let mut info = Vec::with_capacity(8 * 3 + 64);
    info.extend_from_slice(&conversation.to_be_bytes());
    info.extend_from_slice(&first.0.to_be_bytes());
    info.extend_from_slice(&first.1);
    info.extend_from_slice(&second.0.to_be_bytes());
    info.extend_from_slice(&second.1);
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(b"harmony-dm-v1"), shared.as_bytes()).expand(&info, key.as_mut_slice()).ok()?;
    Some(PairKey(key))
}

impl PairKey {
    pub fn seal(&self, aad: &str, plain: &[u8]) -> String {
        b64(&seal_with(&self.0, aad.as_bytes(), plain))
    }

    pub fn open(&self, aad: &str, sealed: &str) -> Option<Vec<u8>> {
        open_with(&self.0, aad.as_bytes(), &unb64(sealed)?)
    }
}

/// The random nonce of something sealed. Fresh for every sealing, so the same one under two
/// message ids is one sealed text sent twice: the client tells a replay by it even for messages
/// sealed before they carried an id of their own.
pub fn nonce_of(sealed: &str) -> Option<[u8; NONCE]> {
    let bytes = unb64(sealed)?;
    (bytes.len() >= 1 + NONCE + TAG && bytes[0] == VERSION).then(|| bytes[1..1 + NONCE].try_into().expect("NONCE bytes"))
}

/// What a sealed thing is and where it belongs, bound in as associated data.
///
/// Which message it is can't be: the server numbers a message only after it is sealed. So that
/// goes inside, in the sealed text, which the AEAD authenticates exactly as it does these (see
/// `dm::Payload`). A call's key is told from another call's by its nonce (`nonce_of`). Keeping
/// these strings as they were is what lets a version from before that change still open
/// everything sent after it.
pub mod aad {
    pub fn message(conversation: i64, sender: i64) -> String {
        format!("harmony-dm-v1/message/{conversation}/{sender}")
    }

    pub fn reaction(conversation: i64, message: i64, user: i64) -> String {
        format!("harmony-dm-v1/reaction/{conversation}/{message}/{user}")
    }

    pub fn call(conversation: i64, caller: i64) -> String {
        format!("harmony-dm-v1/call/{conversation}/{caller}")
    }
}

/// Pads a message to a multiple of 64 bytes (at least 128) with trailing spaces, which JSON
/// ignores, so its sealed size says roughly how long it is and not exactly.
pub fn pad(mut json: Vec<u8>) -> Vec<u8> {
    let target = json.len().max(128).div_ceil(64) * 64;
    json.resize(target, b' ');
    json
}

// Files.

/// Seals a file under a key of its own. The key goes inside the sealed message that sends it.
pub fn seal_file(plain: &[u8]) -> (Vec<u8>, Zeroizing<[u8; 32]>) {
    let key = Zeroizing::new(random());
    (seal_with(&key, b"harmony-dm-v1/file", plain), key)
}

pub fn open_file(key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    open_with(key, b"harmony-dm-v1/file", sealed)
}

// Verification.

/// The safety number of two people's keys: the same on both screens exactly when each sees the
/// other's real key. Thirty digits in six groups, read out loud in a call to compare.
pub fn safety_number(a: (UserId, &[u8; 32]), b: (UserId, &[u8; 32])) -> String {
    let (first, second) = if a.0 < b.0 { (a, b) } else { (b, a) };
    let mut h = Sha256::new();
    h.update(b"harmony-safety-v1");
    h.update(first.0.to_be_bytes());
    h.update(first.1);
    h.update(second.0.to_be_bytes());
    h.update(second.1);
    let digest = h.finalize();
    digest
        .chunks(5)
        .take(6)
        .map(|c| {
            let n = c.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64);
            format!("{:05}", n % 100_000)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_people_agree_on_a_key_and_nobody_else_does() {
        let (alice, bob, eve) = (Identity::generate(), Identity::generate(), Identity::generate());
        let a = pair_key(&alice, 1, &bob.public(), 2, 7).unwrap();
        let b = pair_key(&bob, 2, &alice.public(), 1, 7).unwrap();
        let sealed = a.seal(&aad::message(7, 1), "olá, bob".as_bytes());
        assert_eq!(b.open(&aad::message(7, 1), &sealed).as_deref(), Some("olá, bob".as_bytes()));

        let eavesdropper = pair_key(&eve, 3, &alice.public(), 1, 7).unwrap();
        assert!(eavesdropper.open(&aad::message(7, 1), &sealed).is_none());
        // The same two people in another conversation: another key.
        let elsewhere = pair_key(&bob, 2, &alice.public(), 1, 8).unwrap();
        assert!(elsewhere.open(&aad::message(7, 1), &sealed).is_none());
    }

    #[test]
    fn a_sealed_thing_does_not_open_anywhere_it_was_not_sealed_for() {
        let (alice, bob) = (Identity::generate(), Identity::generate());
        let k = pair_key(&alice, 1, &bob.public(), 2, 7).unwrap();
        let sealed = k.seal(&aad::reaction(7, 100, 1), b"[\"x\"]");
        assert!(k.open(&aad::reaction(7, 101, 1), &sealed).is_none(), "moved to another message");
        assert!(k.open(&aad::reaction(7, 100, 2), &sealed).is_none(), "claimed by the other person");
        assert!(k.open(&aad::message(7, 1), &sealed).is_none(), "passed off as a message");
        let mut bytes = unb64(&sealed).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        assert!(k.open(&aad::reaction(7, 100, 1), &b64(&bytes)).is_none(), "tampered with");
    }

    #[test]
    fn the_same_text_seals_differently_every_time() {
        let (alice, bob) = (Identity::generate(), Identity::generate());
        let k = pair_key(&alice, 1, &bob.public(), 2, 7).unwrap();
        let (one, two) = (k.seal("a", b"same"), k.seal("a", b"same"));
        assert_ne!(one, two);
        assert_ne!(nonce_of(&one), nonce_of(&two));
        assert_eq!(nonce_of(&one).unwrap().as_slice(), &unb64(&one).unwrap()[1..1 + NONCE]);
        assert_eq!(nonce_of("not sealed"), None);
    }

    #[test]
    fn a_small_order_key_is_refused() {
        let alice = Identity::generate();
        assert!(pair_key(&alice, 1, &[0u8; 32], 2, 7).is_none());
    }

    #[test]
    fn recovery_keys_read_back_however_they_were_typed() {
        let r = RecoveryKey::generate();
        let shown = r.display();
        assert_eq!(shown.len(), 39);
        assert_eq!(shown.matches('-').count(), 7);
        let typed = shown.to_lowercase().replace('-', " ").replace('0', "o").replace('1', "l");
        assert_eq!(RecoveryKey::parse(&typed).unwrap().display(), shown);
        assert!(RecoveryKey::parse(&shown[..30]).is_none(), "too short");
        assert!(RecoveryKey::parse(&format!("{shown}A")).is_none(), "too long");
        assert!(RecoveryKey::parse(&shown.replacen(|c: char| c.is_ascii_alphanumeric(), "U", 1)).is_none(), "not in the alphabet");
    }

    #[test]
    fn a_wrapped_identity_opens_only_with_its_recovery_key() {
        let id = Identity::generate();
        let r = RecoveryKey::generate();
        let w = wrap(&id, &r);
        let back = unwrap(&w, &id.public(), &RecoveryKey::parse(&r.display()).unwrap()).unwrap();
        assert_eq!(back.public(), id.public());
        assert!(unwrap(&w, &id.public(), &RecoveryKey::generate()).is_none());
        assert!(unwrap(&w, &Identity::generate().public(), &r).is_none(), "a wrapping is bound to its public key");
    }

    #[test]
    fn files_seal_under_their_own_key() {
        let data = vec![7u8; 100_000];
        let (sealed, key) = seal_file(&data);
        assert_eq!(sealed.len(), data.len() + 1 + NONCE + TAG);
        assert_eq!(open_file(&key, &sealed).unwrap(), data);
        assert!(open_file(&random(), &sealed).is_none());
    }

    #[test]
    fn padding_hides_the_exact_length_and_json_still_parses() {
        let padded = pad(br#"{"body":"hi"}"#.to_vec());
        assert_eq!(padded.len(), 128);
        assert_eq!(pad(vec![b'x'; 129]).len(), 192);
        let v: serde_json::Value = serde_json::from_slice(&padded).unwrap();
        assert_eq!(v["body"], "hi");
    }

    // Known answers. Everything below was produced once by this code and written down: what was
    // sealed and stored with it (messages on the server, wrapped keys, safety numbers people
    // compared) has to keep opening and reading the same, so a change that moves any of these
    // bytes is a change of format, not a refactoring.

    /// Two fixed identities, ids 1 and 2, as the vectors use them.
    fn fixed() -> (Identity, Identity) {
        (Identity::from_secret(Zeroizing::new([0x11; 32])), Identity::from_secret(Zeroizing::new([0x22; 32])))
    }

    #[test]
    fn known_answer_the_pair_key_s_salt_and_info() {
        let (a, b) = fixed();
        assert_eq!(a.public_b64(), "e06Qm75__kTEZaIgA31gjuNYl9Me-XLwf3SJLLD3PxM");
        assert_eq!(b.public_b64(), "D6poTtKIZ7l_Smot7l34zpdOdrcBjj8iocTPJnhXDyA");
        // HKDF-SHA256, salt "harmony-dm-v1", info conversation ‖ lower id ‖ its key ‖ higher id ‖
        // its key, ids as big-endian i64: the same from either side.
        let want = "b0fc31987202b69061f05d8fb6f5209a170391eba63aaa4e056079cbd7050faf";
        assert_eq!(hex::encode(pair_key(&a, 1, &b.public(), 2, 7).unwrap().0.as_slice()), want);
        assert_eq!(hex::encode(pair_key(&b, 2, &a.public(), 1, 7).unwrap().0.as_slice()), want);
        let shared = a.secret.diffie_hellman(&PublicKey::from(b.public()));
        let mut info = 7i64.to_be_bytes().to_vec();
        info.extend(1i64.to_be_bytes().iter().chain(&a.public()).chain(&2i64.to_be_bytes()).chain(&b.public()));
        let mut by_hand = [0u8; 32];
        Hkdf::<Sha256>::new(Some(b"harmony-dm-v1"), shared.as_bytes()).expand(&info, &mut by_hand).unwrap();
        assert_eq!(hex::encode(by_hand), want);
    }

    #[test]
    fn known_answer_the_associated_data_of_each_kind() {
        assert_eq!(aad::message(7, 1), "harmony-dm-v1/message/7/1");
        assert_eq!(aad::reaction(7, 100, 2), "harmony-dm-v1/reaction/7/100/2");
        assert_eq!(aad::call(7, 1), "harmony-dm-v1/call/7/1");
        assert_eq!(aad::message(-3, 9_007_199_254_740_993), "harmony-dm-v1/message/-3/9007199254740993");
    }

    #[test]
    fn known_answer_a_sealed_message_s_bytes() {
        let (a, b) = fixed();
        let sealed = "ATMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM4Fkr1GHru_b3hVjLtqVztcZHeCo3hqSFUr3hzlw";
        let k = pair_key(&b, 2, &a.public(), 1, 7).unwrap();
        assert_eq!(k.open(&aad::message(7, 1), sealed).as_deref(), Some(br#"{"body":"oi"}"#.as_slice()));
        assert!(k.open(&aad::message(7, 2), sealed).is_none());
        // base64url without padding of version 1 ‖ the 24-byte nonce ‖ ciphertext and tag.
        let bytes = unb64(sealed).unwrap();
        assert_eq!(bytes[0], 1);
        assert_eq!(&bytes[1..25], &[0x33; 24]);
        assert_eq!(bytes.len(), 1 + 24 + 13 + 16);
        assert_eq!(nonce_of(sealed), Some([0x33; 24]));
        assert_eq!(b64(&seal_with_nonce(&k.0, aad::message(7, 1).as_bytes(), br#"{"body":"oi"}"#, [0x33; 24])), sealed);
        let mut later = bytes.clone();
        later[0] = 2;
        assert!(k.open(&aad::message(7, 1), &b64(&later)).is_none(), "another version is not read as this one");
    }

    #[test]
    fn known_answer_a_sealed_file() {
        let sealed = unb64("AXd3d3d3d3d3d3d3d3d3d3d3d3d3d3d3dwAHN4bEUCaTQI39Vu7KX_vFf3RZlBpQO1ta").unwrap();
        assert_eq!(open_file(&[0x66; 32], &sealed).as_deref(), Some(b"file bytes".as_slice()));
    }

    #[test]
    fn known_answer_the_recovery_key_and_its_wrapping() {
        let (a, _) = fixed();
        let bytes: [u8; 20] = std::array::from_fn(|i| (i as u8).wrapping_mul(13));
        let shown = "006H-M9SM-8575-PT3N-GA7S-SADP-RF8D-VTQQ";
        assert_eq!(RecoveryKey(Zeroizing::new(bytes)).display(), shown);
        // Crockford's base32 read back, however it was typed.
        for typed in [
            shown,
            "006h m9sm 8575 pt3n ga7s sadp rf8d vtqq",
            "OO6H-M9SM-8575-PT3N-GA7S-SADP-RF8D-VTQQ",
            "006HM9SM8575PT3NGA7SSADPRF8DVTQQ",
        ] {
            assert_eq!(*RecoveryKey::parse(typed).unwrap().0, bytes, "{typed}");
        }
        assert_eq!(
            *RecoveryKey::parse("1111-1111-1111-1111-1111-1111-1111-1111").unwrap().0,
            *RecoveryKey::parse("IiLl-1111-1111-1111-1111-1111-1111-1111").unwrap().0
        );
        let r = RecoveryKey::parse(shown).unwrap();
        // HKDF-SHA256, salt "harmony-recovery-v1", info "wrap identity".
        assert_eq!(hex::encode(r.wrapping_key().as_slice()), "1a1f7e72a5523fe600c53989e36590567655d765c156ff0dc2089825fcdbc1ba");
        // What the server keeps: the private half sealed under it, the public key as associated data.
        let wrapped = "AVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVbWXuqBjNMZkhU8tgZjfsPQ6TJ0qumXZuJddZM4bcF3OtQx0QmCbOScd47xAgzUwvg";
        let back = unwrap(wrapped, &a.public(), &r).unwrap();
        assert_eq!(*back.secret_bytes(), *a.secret_bytes());
        assert_eq!(b64(&seal_with_nonce(&r.wrapping_key(), &a.public(), a.secret_bytes().as_slice(), [0x55; 24])), wrapped);
    }

    #[test]
    fn known_answer_the_safety_number() {
        let (a, b) = fixed();
        let want = "13418 66612 62703 17296 72420 61925";
        assert_eq!(safety_number((1, &a.public()), (2, &b.public())), want);
        assert_eq!(safety_number((2, &b.public()), (1, &a.public())), want);
    }

    #[test]
    fn safety_numbers_match_on_both_sides_and_change_with_a_key() {
        let (a, b, c) = (Identity::generate(), Identity::generate(), Identity::generate());
        let one = safety_number((1, &a.public()), (2, &b.public()));
        assert_eq!(one, safety_number((2, &b.public()), (1, &a.public())));
        assert_ne!(one, safety_number((1, &a.public()), (2, &c.public())));
        assert_eq!(one.len(), 6 * 5 + 5);
        assert!(one.split(' ').all(|g| g.len() == 5 && g.bytes().all(|b| b.is_ascii_digit())));
    }
}
