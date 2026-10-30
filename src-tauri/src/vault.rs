//! Credential vault. Secrets never touch SQLite; `Settings.credential` is just a
//! name that resolves here.
//!
//! Two backends behind one trait:
//! - `KeyringVault` (installed mode): one OS-keyring entry per secret under the
//!   `charon` service. Keyrings cannot enumerate, so a `vault-index.json` beside the
//!   database lists names + kinds (never values).
//! - `FileVault` (portable mode): everything in `data/vault.age`, encrypted with a
//!   master password (age scrypt recipient). The master password is asked for once
//!   per run and lives only in memory.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretKind {
    Password,
    Key,
}

/// A stored secret. For `Key`, `value` is either a path to a private key file or
/// the PEM text itself; `key_passphrase` unlocks it if encrypted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    pub kind: SecretKind,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_passphrase: Option<String>,
}

/// What the UI is allowed to see about a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub name: String,
    pub kind: SecretKind,
}

#[derive(Debug, Clone, Serialize)]
pub struct VaultStatus {
    /// "keyring" or "file".
    pub backend: &'static str,
    /// File vault only: no `vault.age` yet, so unlocking creates it.
    pub initialised: bool,
    pub locked: bool,
}

pub trait Vault: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<Secret>>;
    fn set(&self, name: &str, secret: &Secret) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
    fn list(&self) -> Result<Vec<SecretMeta>>;
    fn status(&self) -> VaultStatus;
    /// Supply the master password. No-op for backends that have none.
    fn unlock(&self, _master: &str) -> Result<()> {
        Ok(())
    }
    /// Give a credential a new name. The new entry is written before the old one
    /// is removed, so a failure part-way leaves the secret under at least one name.
    fn rename(&self, from: &str, to: &str) -> Result<()> {
        check_name(to)?;
        if from == to {
            return Ok(());
        }
        let secret = self.get(from)?.ok_or_else(|| anyhow::anyhow!("no credential named {from:?}"))?;
        if self.get(to)?.is_some() {
            bail!("a credential named {to:?} already exists");
        }
        self.set(to, &secret)?;
        self.delete(from)
    }
}

/// An edit to a stored credential from the UI, which never sees the current values:
/// `None` (or empty) keeps what is stored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SecretEdit {
    pub kind: Option<SecretKind>,
    pub value: Option<String>,
    pub key_passphrase: Option<String>,
    /// Drop a stored key passphrase.
    #[serde(default)]
    pub clear_passphrase: bool,
}

/// Apply `e` to `old`. Changing the kind needs a new value (a password is not a key
/// path); a password never carries a passphrase.
pub fn apply_edit(old: &Secret, e: &SecretEdit) -> Result<Secret> {
    let kind = e.kind.unwrap_or(old.kind);
    let value = match e.value.as_deref().filter(|v| !v.is_empty()) {
        Some(v) => v.to_string(),
        None if kind == old.kind => old.value.clone(),
        None => bail!("changing the kind needs a new {}", if kind == SecretKind::Key { "key path" } else { "password" }),
    };
    let key_passphrase = match kind {
        SecretKind::Password => None,
        SecretKind::Key => match e.key_passphrase.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => Some(p.to_string()),
            None if e.clear_passphrase => None,
            None => old.key_passphrase.clone().filter(|_| old.kind == SecretKind::Key),
        },
    };
    Ok(Secret { kind, value, key_passphrase })
}

fn check_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 200 || name.contains(['/', '\\', '\0']) {
        bail!("invalid credential name {name:?}");
    }
    Ok(())
}

/// Write via a temp file + rename so a crash never leaves a half-written vault.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------- keyring

pub struct KeyringVault {
    service: String,
    index_path: PathBuf,
    index: Mutex<BTreeMap<String, SecretKind>>,
}

impl KeyringVault {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let index_path = data_dir.join("vault-index.json");
        let index = match std::fs::read(&index_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("parse vault-index.json")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).context("read vault-index.json"),
        };
        Ok(Self { service: "charon".into(), index_path, index: Mutex::new(index) })
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(&self.service, name)?)
    }

    fn save_index(&self, index: &BTreeMap<String, SecretKind>) -> Result<()> {
        write_atomic(&self.index_path, &serde_json::to_vec_pretty(index)?)
    }
}

impl Vault for KeyringVault {
    fn get(&self, name: &str) -> Result<Option<Secret>> {
        check_name(name)?;
        match self.entry(name)?.get_password() {
            Ok(json) => Ok(Some(serde_json::from_str(&json).context("parse keyring entry")?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, name: &str, secret: &Secret) -> Result<()> {
        check_name(name)?;
        self.entry(name)?.set_password(&serde_json::to_string(secret)?)?;
        let mut index = self.index.lock().unwrap();
        index.insert(name.to_string(), secret.kind);
        self.save_index(&index)
    }

    fn delete(&self, name: &str) -> Result<()> {
        check_name(name)?;
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(e.into()),
        }
        let mut index = self.index.lock().unwrap();
        index.remove(name);
        self.save_index(&index)
    }

    fn list(&self) -> Result<Vec<SecretMeta>> {
        Ok(self
            .index
            .lock()
            .unwrap()
            .iter()
            .map(|(name, kind)| SecretMeta { name: name.clone(), kind: *kind })
            .collect())
    }

    fn status(&self) -> VaultStatus {
        VaultStatus { backend: "keyring", initialised: true, locked: false }
    }
}

// ---------------------------------------------------------------- file (age)

struct Unlocked {
    master: age::secrecy::SecretString,
    secrets: BTreeMap<String, Secret>,
}

pub struct FileVault {
    path: PathBuf,
    state: Mutex<Option<Unlocked>>,
}

impl FileVault {
    pub fn open(data_dir: &Path) -> Self {
        Self { path: data_dir.join("vault.age"), state: Mutex::new(None) }
    }

    fn encrypt(master: &age::secrecy::SecretString, secrets: &BTreeMap<String, Secret>) -> Result<Vec<u8>> {
        let plain = serde_json::to_vec(secrets)?;
        let mut out = Vec::new();
        let mut w = age::Encryptor::with_user_passphrase(master.clone()).wrap_output(&mut out)?;
        w.write_all(&plain)?;
        w.finish()?;
        Ok(out)
    }

    fn decrypt(master: &age::secrecy::SecretString, bytes: &[u8]) -> Result<BTreeMap<String, Secret>> {
        let plain = age::decrypt(&age::scrypt::Identity::new(master.clone()), bytes)
            .map_err(|e| match e {
                age::DecryptError::DecryptionFailed => anyhow!("wrong master password"),
                other => anyhow!("decrypt vault.age: {other}"),
            })?;
        Ok(serde_json::from_slice(&plain).context("parse decrypted vault")?)
    }

    fn with_unlocked<T>(&self, f: impl FnOnce(&mut Unlocked) -> Result<T>) -> Result<T> {
        let mut guard = self.state.lock().unwrap();
        let u = guard.as_mut().ok_or_else(|| anyhow!("vault is locked"))?;
        f(u)
    }

    fn persist(&self, u: &Unlocked) -> Result<()> {
        write_atomic(&self.path, &Self::encrypt(&u.master, &u.secrets)?)
    }
}

impl Vault for FileVault {
    fn get(&self, name: &str) -> Result<Option<Secret>> {
        check_name(name)?;
        self.with_unlocked(|u| Ok(u.secrets.get(name).cloned()))
    }

    fn set(&self, name: &str, secret: &Secret) -> Result<()> {
        check_name(name)?;
        self.with_unlocked(|u| {
            u.secrets.insert(name.to_string(), secret.clone());
            self.persist(u)
        })
    }

    fn delete(&self, name: &str) -> Result<()> {
        check_name(name)?;
        self.with_unlocked(|u| {
            u.secrets.remove(name);
            self.persist(u)
        })
    }

    fn list(&self) -> Result<Vec<SecretMeta>> {
        self.with_unlocked(|u| {
            Ok(u.secrets.iter().map(|(n, s)| SecretMeta { name: n.clone(), kind: s.kind }).collect())
        })
    }

    fn status(&self) -> VaultStatus {
        VaultStatus {
            backend: "file",
            initialised: self.path.exists(),
            locked: self.state.lock().unwrap().is_none(),
        }
    }

    /// Decrypts an existing `vault.age`, or creates an empty one if none exists.
    fn unlock(&self, master: &str) -> Result<()> {
        if master.is_empty() {
            bail!("master password must not be empty");
        }
        let master = age::secrecy::SecretString::from(master.to_string());
        let secrets = match std::fs::read(&self.path) {
            Ok(bytes) => Self::decrypt(&master, &bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).context("read vault.age"),
        };
        let u = Unlocked { master, secrets };
        if !self.path.exists() {
            self.persist(&u)?;
        }
        *self.state.lock().unwrap() = Some(u);
        Ok(())
    }
}

/// Pick the backend for this install.
pub fn open(data_dir: &Path, portable: bool) -> Result<Box<dyn Vault>> {
    Ok(if portable {
        Box::new(FileVault::open(data_dir))
    } else {
        Box::new(KeyringVault::open(data_dir)?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pw(v: &str) -> Secret {
        Secret { kind: SecretKind::Password, value: v.into(), key_passphrase: None }
    }

    #[test]
    fn file_vault_locked_until_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path());
        assert!(v.status().locked);
        assert!(!v.status().initialised);
        assert!(v.list().is_err());
        assert!(v.set("x", &pw("y")).is_err());
    }

    #[test]
    fn file_vault_round_trips_through_vault_age() {
        let dir = tempfile::tempdir().unwrap();
        {
            let v = FileVault::open(dir.path());
            v.unlock("hunter2").unwrap();
            assert!(v.status().initialised);
            v.set("prod-root", &pw("s3cret")).unwrap();
            v.set(
                "lab-key",
                &Secret { kind: SecretKind::Key, value: "/home/t/.ssh/id_ed25519".into(), key_passphrase: Some("kp".into()) },
            )
            .unwrap();
        }
        assert!(dir.path().join("vault.age").exists());
        assert!(!dir.path().join("vault.tmp").exists());
        // Ciphertext must not leak the plaintext.
        let raw = std::fs::read(dir.path().join("vault.age")).unwrap();
        assert!(!raw.windows(6).any(|w| w == b"s3cret"));

        // Fresh instance, same master: everything is back.
        let v = FileVault::open(dir.path());
        v.unlock("hunter2").unwrap();
        assert_eq!(v.get("prod-root").unwrap(), Some(pw("s3cret")));
        assert_eq!(v.get("lab-key").unwrap().unwrap().key_passphrase.as_deref(), Some("kp"));
        assert_eq!(v.get("nope").unwrap(), None);
        let mut names: Vec<_> = v.list().unwrap().into_iter().map(|m| m.name).collect();
        names.sort();
        assert_eq!(names, vec!["lab-key", "prod-root"]);

        v.delete("prod-root").unwrap();
        assert_eq!(v.get("prod-root").unwrap(), None);
        assert_eq!(v.list().unwrap().len(), 1);
    }

    #[test]
    fn rename_moves_the_secret_and_refuses_clashes() {
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path());
        v.unlock("m").unwrap();
        v.set("old", &pw("s")).unwrap();
        v.set("taken", &pw("t")).unwrap();
        assert!(v.rename("old", "taken").is_err());
        assert!(v.rename("old", "bad/name").is_err());
        assert!(v.rename("nope", "x").is_err());
        v.rename("old", "new").unwrap();
        assert_eq!(v.get("new").unwrap().unwrap().value, "s");
        assert!(v.get("old").unwrap().is_none());
        assert_eq!(v.get("taken").unwrap().unwrap().value, "t");
    }

    #[test]
    fn file_vault_rejects_wrong_master() {
        let dir = tempfile::tempdir().unwrap();
        FileVault::open(dir.path()).unlock("right").unwrap();
        let v = FileVault::open(dir.path());
        let err = v.unlock("wrong").unwrap_err().to_string();
        assert!(err.contains("wrong master password"), "{err}");
        assert!(v.status().locked);
    }

    #[test]
    fn names_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path());
        v.unlock("m").unwrap();
        assert!(v.set("", &pw("x")).is_err());
        assert!(v.set("a/b", &pw("x")).is_err());
        assert!(v.set("ok name", &pw("x")).is_ok());
    }

    #[test]
    fn keyring_index_lists_without_touching_keyring() {
        // The index is plain JSON of name → kind; make sure it never carries values.
        let dir = tempfile::tempdir().unwrap();
        let idx: BTreeMap<String, SecretKind> = [("a".to_string(), SecretKind::Key)].into();
        std::fs::write(dir.path().join("vault-index.json"), serde_json::to_vec(&idx).unwrap()).unwrap();
        let v = KeyringVault::open(dir.path()).unwrap();
        assert_eq!(v.list().unwrap(), vec![SecretMeta { name: "a".into(), kind: SecretKind::Key }]);
        assert!(!v.status().locked);
    }

    #[test]
    fn edits_keep_what_is_left_blank() {
        let key = Secret { kind: SecretKind::Key, value: "/k/id".into(), key_passphrase: Some("pp".into()) };
        // nothing given: unchanged
        assert_eq!(apply_edit(&key, &SecretEdit::default()).unwrap(), key);
        // new path, passphrase kept
        let e = SecretEdit { value: Some("/k/other".into()), ..Default::default() };
        assert_eq!(apply_edit(&key, &e).unwrap(), Secret { value: "/k/other".into(), ..key.clone() });
        // new passphrase / cleared passphrase
        let e = SecretEdit { key_passphrase: Some("new".into()), ..Default::default() };
        assert_eq!(apply_edit(&key, &e).unwrap().key_passphrase.as_deref(), Some("new"));
        let e = SecretEdit { clear_passphrase: true, ..Default::default() };
        assert_eq!(apply_edit(&key, &e).unwrap().key_passphrase, None);
        // switching kind needs a value, and a password drops the passphrase
        let e = SecretEdit { kind: Some(SecretKind::Password), ..Default::default() };
        assert!(apply_edit(&key, &e).is_err());
        let e = SecretEdit { kind: Some(SecretKind::Password), value: Some("hunter2".into()), ..Default::default() };
        assert_eq!(apply_edit(&key, &e).unwrap(), pw("hunter2"));
        // password → key does not invent a passphrase
        let e = SecretEdit { kind: Some(SecretKind::Key), value: Some("/k/id".into()), ..Default::default() };
        assert_eq!(apply_edit(&pw("x"), &e).unwrap().key_passphrase, None);
    }
}
