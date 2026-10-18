//! Secrets at rest. On Windows they are sealed with DPAPI for the signed-in Windows user, so a
//! copy of `settings.json` on another account or computer holds no usable password or sign-in.
//! On macOS and Linux they are sealed with XChaCha20-Poly1305 under a random key of Ciranda's own
//! that lives in the system's keychain (the login keychain; the Secret Service, as GNOME Keyring
//! or KWallet provide it), to the same effect. Where there is no keychain to ask (a bare window
//! manager with no Secret Service, a keychain that said no) they are kept as they are, as before.

use super::crypto::random;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use zeroize::Zeroizing;

const DPAPI: &str = "dpapi:";
const KEYCHAIN: &str = "keychain:";

/// Whether this system has a way to seal: DPAPI, or a keychain. `seals` says whether it does on
/// this run.
#[cfg_attr(not(test), allow(dead_code))]
pub const SEALS: bool = cfg!(any(windows, target_os = "macos", target_os = "linux"));

/// Whether `seal` really seals on this run.
pub fn seals() -> bool {
    cfg!(windows) || key().is_some()
}

/// What the file holds for a secret. Empty stays empty, so a missing secret looks missing; and
/// if sealing fails the value is kept as it is rather than lost with the sign-in it carries.
pub fn seal(plain: &str) -> String {
    if plain.is_empty() {
        return plain.to_string();
    }
    if cfg!(windows) {
        return match dpapi::protect(plain.as_bytes()) {
            Some(blob) => format!("{DPAPI}{}", hex::encode(blob)),
            None => {
                log::warn!("settings: a secret could not be sealed; it is kept unsealed");
                plain.to_string()
            }
        };
    }
    seal_with(key(), plain)
}

pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(DPAPI) || stored.starts_with(KEYCHAIN)
}

/// The secret behind what the file holds. Anything unsealed is a value from before sealing and is
/// taken as it is; `None` when a sealed one does not open (another user's, damaged, or `locked`).
pub fn open(stored: &str) -> Option<String> {
    if let Some(blob) = stored.strip_prefix(DPAPI) {
        return String::from_utf8(dpapi::unprotect(&hex::decode(blob).ok()?)?).ok();
    }
    open_with(key(), stored)
}

/// Sealed under a keychain key this run does not have: the keychain did not answer, or answered
/// with another key. Such a value is not damaged, and is worth keeping for a run that has it.
pub fn locked(stored: &str) -> bool {
    locked_with(key(), stored)
}

/// Asks the keychain for the key, making one the first time. At start-up, before the window: a
/// keychain may show a prompt (macOS asks once for a program whose signature changed; a locked
/// GNOME Keyring asks for its password), and the answer is waited for up to `WAIT`. One that comes
/// later still counts from then on. Without one, secrets stay as they are on this run.
pub fn init() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        const WAIT: std::time::Duration = std::time::Duration::from_secs(30);
        let (done, wait) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("ciranda-keychain".into()).spawn(move || {
            let key = match keychain::fetch() {
                Ok(key) => Some(Key::new(key)),
                Err(e) => {
                    log::warn!("keychain: {e}; passwords and sign-ins are kept unsealed on this run");
                    None
                }
            };
            let _ = KEY.set(key);
            let _ = done.send(());
        });
        match spawned {
            Ok(_) if wait.recv_timeout(WAIT).is_err() => {
                log::warn!("keychain: no answer yet; secrets are kept as they are until it comes")
            }
            Ok(_) => {}
            Err(e) => log::warn!("keychain: no thread to ask it ({e}); secrets are kept unsealed"),
        }
    }
}

/// The key secrets are sealed with here, and a short id of it written beside each sealed value, so
/// one sealed under another key (another keychain, a key since replaced) is told from one that is
/// damaged.
struct Key {
    key: Zeroizing<[u8; 32]>,
    id: [u8; ID],
}

const ID: usize = 4;
const NONCE: usize = 24;
/// Bound in as associated data: a value sealed for anything else under the same key won't open.
const AAD: &[u8] = b"ciranda-secret-v1";

impl Key {
    #[cfg_attr(windows, allow(dead_code))]
    fn new(key: [u8; 32]) -> Key {
        let key = Zeroizing::new(key);
        let digest = Sha256::new().chain_update(b"ciranda-secret-key-id").chain_update(key.as_slice()).finalize();
        let mut id = [0; ID];
        id.copy_from_slice(&digest[..ID]);
        Key { key, id }
    }

    /// `id ‖ nonce ‖ ciphertext+tag`.
    fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let nonce: [u8; NONCE] = random();
        let sealed = XChaCha20Poly1305::new(self.key.as_slice().into())
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: AAD })
            .expect("XChaCha20-Poly1305 does not fail to seal");
        [&self.id[..], &nonce, &sealed].concat()
    }

    fn open(&self, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < ID + NONCE + 16 || sealed[..ID] != self.id {
            return None;
        }
        XChaCha20Poly1305::new(self.key.as_slice().into())
            .decrypt(XNonce::from_slice(&sealed[ID..ID + NONCE]), Payload { msg: &sealed[ID + NONCE..], aad: AAD })
            .ok()
    }
}

static KEY: OnceLock<Option<Key>> = OnceLock::new();

/// The keychain's key, once it has answered. Tests seal under one of their own and never touch
/// the system's keychain.
fn key() -> Option<&'static Key> {
    #[cfg(test)]
    return KEY.get_or_init(|| Some(Key::new(random()))).as_ref();
    #[cfg(not(test))]
    KEY.get().and_then(Option::as_ref)
}

fn seal_with(key: Option<&Key>, plain: &str) -> String {
    match key {
        Some(key) if !plain.is_empty() => format!("{KEYCHAIN}{}", hex::encode(key.seal(plain.as_bytes()))),
        _ => plain.to_string(),
    }
}

fn open_with(key: Option<&Key>, stored: &str) -> Option<String> {
    let Some(blob) = stored.strip_prefix(KEYCHAIN) else { return Some(stored.to_string()) };
    String::from_utf8(key?.open(&hex::decode(blob).ok()?)?).ok()
}

fn locked_with(key: Option<&Key>, stored: &str) -> bool {
    let Some(id) = stored.strip_prefix(KEYCHAIN).and_then(|blob| hex::decode(blob.get(..ID * 2)?).ok()) else { return false };
    key.is_none_or(|k| k.id[..] != id[..])
}

#[cfg(target_os = "macos")]
mod keychain {
    use security_framework::passwords::{PasswordOptions, generic_password, set_generic_password};

    const SERVICE: &str = "Ciranda";
    const ACCOUNT: &str = "secrets key";
    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;

    /// The key in the login keychain, or a new one put there. The item is Ciranda's: macOS lets
    /// the program that made it read it without asking, and asks once when that program's
    /// signature changes.
    pub fn fetch() -> Result<[u8; 32], String> {
        match generic_password(PasswordOptions::new_generic_password(SERVICE, ACCOUNT)) {
            Ok(bytes) => bytes.try_into().map_err(|_| "the key in the keychain is not 32 bytes".to_string()),
            Err(e) if e.code() == NOT_FOUND => {
                let key = super::random::<32>();
                set_generic_password(SERVICE, ACCOUNT, &key).map_err(|e| e.to_string())?;
                Ok(key)
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
mod keychain {
    const LABEL: &str = "Ciranda";
    const ATTRIBUTES: [(&str, &str); 2] = [("application", "ciranda"), ("purpose", "secrets-key")];

    /// The key in the Secret Service's default collection, or a new one put there. zbus drives
    /// its own connection, so any thread can wait on it.
    pub fn fetch() -> Result<[u8; 32], String> {
        futures::executor::block_on(async {
            let keyring = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
            keyring.unlock().await.map_err(|e| e.to_string())?;
            if let Some(item) = keyring.search_items(&ATTRIBUTES).await.map_err(|e| e.to_string())?.first() {
                item.unlock().await.map_err(|e| e.to_string())?;
                let secret = item.secret().await.map_err(|e| e.to_string())?;
                return <[u8; 32]>::try_from(&secret[..]).map_err(|_| "the key in the keyring is not 32 bytes".to_string());
            }
            let key = super::random::<32>();
            keyring.create_item(LABEL, &ATTRIBUTES, &key, true).await.map_err(|e| e.to_string())?;
            Ok(key)
        })
    }
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData};

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        run(data, false)
    }

    fn run(data: &[u8], seal: bool) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: u32::try_from(data.len()).ok()?, pbData: data.as_ptr().cast_mut() };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        use std::ptr::{null, null_mut};
        // SAFETY: `input` points at `data`, alive for the call, and DPAPI only reads it. On success
        // the system allocates `out.pbData`, which is copied out and then handed back to LocalFree.
        let ok = unsafe {
            if seal {
                CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&input, null_mut(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            }
        };
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        // SAFETY: as above; `out` is the system's buffer of `cbData` bytes until LocalFree.
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData.cast()) };
        Some(bytes)
    }
}

#[cfg(not(windows))]
mod dpapi {
    pub fn protect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn unprotect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_round_trip_and_old_plain_values_read_as_they_are() {
        let sealed = seal("hunter22");
        assert_eq!(open(&sealed).as_deref(), Some("hunter22"));
        assert!(is_sealed(&sealed) && seals());
        assert!(!sealed.contains("hunter22"));
        assert_eq!(seal(""), "");
        assert_eq!(open("from before").as_deref(), Some("from before"));
        assert_eq!(open("dpapi:00ff"), None);
        assert_eq!(open("dpapi:not hex"), None);
    }

    #[test]
    fn keychain_values_open_only_under_their_own_key() {
        let (a, b) = (Key::new([1; 32]), Key::new([2; 32]));
        let sealed = seal_with(Some(&a), "tok");
        assert!(sealed.starts_with(KEYCHAIN) && is_sealed(&sealed) && !sealed.contains("tok"));
        assert_ne!(seal_with(Some(&a), "tok"), sealed, "a fresh nonce every time");
        assert_eq!(open_with(Some(&a), &sealed).as_deref(), Some("tok"));
        assert!(!locked_with(Some(&a), &sealed));

        // Another key, or none at all: kept for later rather than taken for damage.
        assert_eq!(open_with(Some(&b), &sealed), None);
        assert!(locked_with(Some(&b), &sealed) && locked_with(None, &sealed));
        assert_eq!(open_with(None, &sealed), None);

        // Damaged under the right key: not locked, just gone.
        let mut bytes = hex::decode(&sealed[KEYCHAIN.len()..]).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        let damaged = format!("{KEYCHAIN}{}", hex::encode(bytes));
        assert_eq!(open_with(Some(&a), &damaged), None);
        assert!(!locked_with(Some(&a), &damaged));
        assert_eq!(open_with(Some(&a), "keychain:zz"), None);
    }

    #[test]
    fn without_a_keychain_secrets_stay_as_they_were() {
        assert_eq!(seal_with(None, "tok"), "tok");
        assert_eq!(seal_with(Some(&Key::new([1; 32])), ""), "");
        // Plain values from before sealing open as they are, and get sealed at the next save.
        assert_eq!(open_with(None, "tok").as_deref(), Some("tok"));
        assert!(!locked_with(None, "tok") && !locked_with(None, "dpapi:00ff"));
    }
}
