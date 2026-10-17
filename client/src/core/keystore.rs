//! Your private-conversation keys on this computer: one small file per server and account, under
//! `keys\` beside the settings. The identity key and the recovery key in it are sealed with DPAPI
//! for this Windows user (see `secret`), like the passwords in the settings file.
//!
//! Also kept here, because they are facts about what this computer has seen rather than settings:
//! the key each person had the first time you talked to them (to notice when it changes), the ones
//! accepted for them since, and the ones you have checked with them in person (the safety number).
//!
//! A file of its own, not a part of `settings.json`: that file is rewritten on every slider step,
//! and this one should be written only when a key actually changes.
//!
//! What 4.3.6 didn't keep (the identity keys this account had before, and the keys accepted besides
//! the pinned one) lives in a second file beside it, `<name>.kept.json`. 4.3.6 rewrites the first
//! file whole with only the fields it knows, so anything kept only there would be lost the first
//! time somebody went back to it; the second file it never opens.

use super::api::normalize_base;
use super::crypto::{Identity, RecoveryKey};
use super::secret;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct Data {
    /// Which server and account this is, for a person looking in the folder.
    server: String,
    username: String,
    key_id: i64,
    /// The identity key's private half, hex, sealed.
    secret: String,
    /// The recovery key as shown, sealed, so it can be shown again from the settings.
    recovery: String,
    /// The person said they put the recovery key somewhere safe.
    recovery_saved: bool,
    /// By user id: the public key to compare theirs against (the first seen, or the last accepted).
    pins: BTreeMap<i64, String>,
    /// By user id: the public key whose safety number they compared with you.
    verified: BTreeMap<i64, String>,
    /// Whatever a later version wrote that this one doesn't know, kept as it was.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// The part 4.3.6 never sees: see the module's notes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct Kept {
    /// Identity keys this account had before the current one, by key id, sealed like `secret`:
    /// what was sealed to them stays readable here.
    archived: BTreeMap<i64, String>,
    /// By user id: every other public key of theirs that was accepted here, so what they sealed
    /// before a key change still opens without a warning.
    accepted: BTreeMap<i64, Vec<String>>,
    /// By user id: the key pinned for them when this computer came from 4.3.6, which opened every
    /// key of theirs up to that one without a word.
    upgraded: BTreeMap<i64, String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

pub struct KeyFile {
    path: PathBuf,
    data: Data,
    kept: Kept,
}

impl KeyFile {
    pub fn open(server: &str, username: &str) -> KeyFile {
        let digest = Sha256::digest(format!("{}\n{}", normalize_base(server).to_lowercase(), username.to_lowercase()));
        let path = super::settings::data_dir().join("keys").join(format!("{}.json", hex::encode(&digest[..12])));
        let mut file = KeyFile::open_at(path);
        file.data.server = normalize_base(server);
        file.data.username = username.to_string();
        file
    }

    pub fn open_at(path: PathBuf) -> KeyFile {
        let mut data: Data = read(&path).unwrap_or_default();
        let kept = read::<Kept>(&kept_path(&path));
        let fresh = kept.is_none();
        let mut kept = kept.unwrap_or_default();
        if fresh {
            kept.upgraded = data.pins.clone();
        }
        // A build before this one kept these in the main file: they move to the one 4.3.6 leaves alone.
        let mut moved = false;
        if let Some(v) = data.extra.remove("archived") {
            kept.archived.extend(serde_json::from_value::<BTreeMap<i64, String>>(v).unwrap_or_default());
            moved = true;
        }
        if let Some(v) = data.extra.remove("accepted") {
            for (user, keys) in serde_json::from_value::<BTreeMap<i64, Vec<String>>>(v).unwrap_or_default() {
                kept.accepted.entry(user).or_default().extend(keys);
            }
            moved = true;
        }
        moved |= data.extra.remove("newer").is_some();
        let file = KeyFile { path, data, kept };
        if fresh || moved {
            file.save_kept();
        }
        if moved {
            file.save();
        }
        file
    }

    /// The identity key and its id on the server, if this computer has it and it opens.
    pub fn identity(&self) -> Option<(i64, Identity)> {
        if self.data.key_id == 0 {
            return None;
        }
        Some((self.data.key_id, unseal_identity(&self.data.secret)?))
    }

    /// An identity key this account had before, if this computer kept it.
    pub fn archived_identity(&self, key_id: i64) -> Option<Identity> {
        unseal_identity(self.kept.archived.get(&key_id)?)
    }

    /// Moves the current identity key to the archive, saved there before it leaves the main file.
    fn archive_current(&mut self) {
        if self.data.key_id != 0 && !self.data.secret.is_empty() {
            self.kept.archived.insert(self.data.key_id, self.data.secret.clone());
            self.save_kept();
        }
    }

    /// The account's key from now on. One it replaces is archived, not dropped.
    pub fn set_identity(&mut self, key_id: i64, identity: &Identity) {
        if self.data.key_id != key_id {
            self.archive_current();
        }
        self.data.key_id = key_id;
        self.data.secret = secret::seal(&Zeroizing::new(hex::encode(identity.secret_bytes().as_slice())));
        self.save();
    }

    /// Stops using the identity key here, as when the server says this account has none. It is
    /// archived rather than deleted: the server may have lost it, but what was sealed to it is
    /// still somewhere, and only this key opens it.
    pub fn retire_identity(&mut self) {
        self.archive_current();
        self.data.key_id = 0;
        self.data.secret.clear();
        self.data.recovery.clear();
        self.data.recovery_saved = false;
        self.save();
    }

    pub fn recovery(&self) -> Option<RecoveryKey> {
        RecoveryKey::parse(&Zeroizing::new(secret::open(&self.data.recovery)?))
    }

    pub fn set_recovery(&mut self, recovery: &RecoveryKey, saved: bool) {
        self.data.recovery = secret::seal(&Zeroizing::new(recovery.display()));
        self.data.recovery_saved = saved;
        self.save();
    }

    pub fn recovery_saved(&self) -> bool {
        self.data.recovery_saved || self.data.recovery.is_empty()
    }

    pub fn mark_recovery_saved(&mut self) {
        self.data.recovery_saved = true;
        self.save();
    }

    pub fn pin(&self, user: i64) -> Option<&str> {
        self.data.pins.get(&user).map(String::as_str)
    }

    /// The key to compare `user`'s against from now on. The one it replaces stays accepted.
    pub fn set_pin(&mut self, user: i64, key: &str) {
        let old = self.data.pins.insert(user, key.to_string());
        if old.as_deref() == Some(key) {
            return;
        }
        if let Some(old) = old {
            self.accept(user, &[&old]);
        }
        self.save();
    }

    /// `key` is one of `user`'s that was accepted here: the pinned one, or one accepted since.
    pub fn accepted(&self, user: i64, key: &str) -> bool {
        self.pin(user) == Some(key) || self.kept.accepted.get(&user).is_some_and(|keys| keys.iter().any(|k| k == key))
    }

    pub fn accept(&mut self, user: i64, keys: &[&str]) {
        let mut fresh: Vec<String> = keys.iter().filter(|k| !self.accepted(user, k)).map(|k| k.to_string()).collect();
        fresh.dedup();
        if !fresh.is_empty() {
            self.kept.accepted.entry(user).or_default().extend(fresh);
            self.save_kept();
        }
    }

    /// The key pinned for `user` by 4.3.6, if this computer came from it with one.
    pub fn upgraded_pin(&self, user: i64) -> Option<&str> {
        self.kept.upgraded.get(&user).map(String::as_str)
    }

    pub fn verified(&self, user: i64) -> Option<&str> {
        self.data.verified.get(&user).map(String::as_str)
    }

    pub fn set_verified(&mut self, user: i64, key: Option<&str>) {
        match key {
            Some(k) => self.data.verified.insert(user, k.to_string()),
            None => self.data.verified.remove(&user),
        };
        self.save();
    }

    /// Written whole to a file beside it, then moved over it, so a crash halfway never leaves a
    /// key file that does not read.
    fn save(&self) {
        write(&self.path, &self.data);
    }

    fn save_kept(&self) {
        write(&kept_path(&self.path), &self.kept);
    }
}

fn kept_path(path: &Path) -> PathBuf {
    path.with_extension("kept.json")
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write(path: &Path, value: &impl Serialize) {
    let Ok(json) = serde_json::to_vec_pretty(value) else { return };
    if let Err(e) = super::settings::write_durably(path, &json) {
        log::warn!("could not save the private-message keys: {e}");
    }
}

fn unseal_identity(sealed: &str) -> Option<Identity> {
    let hexed = Zeroizing::new(secret::open(sealed)?);
    let bytes = Zeroizing::new(hex::decode(hexed.as_str()).ok()?);
    let secret: [u8; 32] = bytes.as_slice().try_into().ok()?;
    Some(Identity::from_secret(Zeroizing::new(secret)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file() -> PathBuf {
        std::env::temp_dir().join(format!("ciranda-keys-{}.json", hex::encode(super::super::crypto::random::<8>())))
    }

    fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(kept_path(path));
    }

    #[test]
    fn keys_survive_a_restart_and_stay_sealed_on_disk() {
        let path = temp_file();
        let id = Identity::generate();
        let r = RecoveryKey::generate();
        {
            let mut f = KeyFile::open_at(path.clone());
            assert!(f.identity().is_none());
            f.set_identity(42, &id);
            f.set_recovery(&r, false);
            f.set_pin(7, "abc");
        }
        let f = KeyFile::open_at(path.clone());
        let (key_id, back) = f.identity().unwrap();
        assert_eq!(key_id, 42);
        assert_eq!(back.public(), id.public());
        assert_eq!(f.recovery().unwrap().display(), r.display());
        assert!(!f.recovery_saved());
        assert_eq!(f.pin(7), Some("abc"));
        let on_disk = std::fs::read_to_string(&path).unwrap();
        if secret::SEALS {
            assert!(!on_disk.contains(&r.display()));
            assert!(!on_disk.contains(&hex::encode(id.secret_bytes().as_slice())));
        }
        let mut f = f;
        f.retire_identity();
        let f = KeyFile::open_at(path.clone());
        assert!(f.identity().is_none());
        assert_eq!(f.archived_identity(42).unwrap().public(), id.public(), "retired, not deleted");
        remove(&path);
    }

    #[test]
    fn a_replaced_identity_is_archived_by_its_key_id() {
        let path = temp_file();
        let (one, two) = (Identity::generate(), Identity::generate());
        let mut f = KeyFile::open_at(path.clone());
        f.set_identity(1, &one);
        f.set_identity(2, &two);
        let f = KeyFile::open_at(path.clone());
        assert_eq!(f.identity().unwrap().0, 2);
        assert_eq!(f.archived_identity(1).unwrap().public(), one.public());
        assert!(f.archived_identity(2).is_none());
        remove(&path);
    }

    /// The key file as 4.3.6 reads and writes it: these fields and no others.
    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    #[derive(Default)]
    struct Old {
        server: String,
        username: String,
        key_id: i64,
        secret: String,
        recovery: String,
        recovery_saved: bool,
        pins: BTreeMap<i64, String>,
        verified: BTreeMap<i64, String>,
    }

    #[test]
    fn going_back_to_4_3_6_and_forward_again_loses_nothing() {
        let path = temp_file();
        let (one, two) = (Identity::generate(), Identity::generate());
        let mut f = KeyFile::open_at(path.clone());
        f.set_identity(1, &one);
        f.set_identity(2, &two);
        f.set_pin(7, "first");
        f.set_pin(7, "second");
        f.data.extra.insert("later".into(), Value::from(5));
        f.save();

        // 4.3.6 opens the file and saves it as it knows it.
        let old: Old = read(&path).unwrap();
        write(&path, &old);

        let f = KeyFile::open_at(path.clone());
        assert_eq!(f.identity().unwrap().0, 2);
        assert_eq!(f.archived_identity(1).unwrap().public(), one.public());
        assert!(f.accepted(7, "first") && f.accepted(7, "second"));
        drop(f);

        // And what a later version wrote survives a save by this one.
        let mut f = KeyFile::open_at(path.clone());
        f.data.extra.insert("later".into(), Value::from(5));
        f.save();
        f.mark_recovery_saved();
        assert_eq!(KeyFile::open_at(path.clone()).data.extra.get("later"), Some(&Value::from(5)));
        remove(&path);
    }

    #[test]
    fn a_4_3_6_file_is_read_and_moved_on() {
        let path = temp_file();
        let old = Old { key_id: 3, pins: BTreeMap::from([(7, "theirs".to_string())]), ..Default::default() };
        let mut json = serde_json::to_value(&old).unwrap();
        // What a build between the two wrote into the main file.
        json["archived"] = serde_json::json!({ "1": "x" });
        json["accepted"] = serde_json::json!({ "7": ["older"] });
        json["newer"] = serde_json::json!(["k"]);
        write(&path, &json);
        let f = KeyFile::open_at(path.clone());
        assert_eq!(f.upgraded_pin(7), Some("theirs"));
        assert!(f.accepted(7, "older"));
        assert!(f.kept.archived.contains_key(&1));
        assert!(f.data.extra.is_empty());
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("archived") && !on_disk.contains("newer"));
        assert_eq!(KeyFile::open_at(path.clone()).upgraded_pin(7), Some("theirs"), "decided once, on the first open");
        remove(&path);
    }
}
