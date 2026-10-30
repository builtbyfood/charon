//! Importers for other tools' connection lists. Each parser is a pure function
//! from text (or a key/value bag) to `Parsed`, so it can be unit-tested with the
//! fixtures next to it; `apply` is the only piece that touches the store/vault.
//!
//! Passwords are never imported. Identity files become `key` vault entries that
//! point at the path; the node's `credential` refers to that entry.

pub mod mobaxterm;
pub mod putty;
pub mod ssh_config;

use crate::connections::{Kind, Node, Settings, Store};
use crate::vault::{Secret, SecretKind, Vault};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportedHost {
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
    pub jump_host: Option<String>,
    /// Folder path inside the import folder (MobaXterm `SubRep`).
    pub folder: Vec<String>,
    /// Terminal font family and size in px (PuTTY `Font` / `FontHeight`).
    pub font: Option<String>,
    pub font_size: Option<u16>,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub hosts: Vec<ImportedHost>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub added: u32,
    pub skipped: u32,
    /// Existing nodes whose settings were overwritten.
    pub updated: u32,
    pub warnings: Vec<String>,
}

/// What `apply` does with a parsed host that matches an existing node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DupPolicy {
    /// Leave the existing node alone, don't add. The default.
    #[default]
    Skip,
    /// Replace the existing node's imported fields (host/port/user/key/jump) in place.
    Overwrite,
    /// Add anyway; a name clash in the target folder gets a " (2)" suffix.
    Copy,
}

/// Shown before applying: what would be added and what already exists.
#[derive(Debug, Default, Serialize)]
pub struct ImportPreview {
    pub new: Vec<String>,
    pub duplicates: Vec<Duplicate>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Duplicate {
    pub name: String,
    pub host: String,
    /// Name of the existing node it matched.
    pub existing: String,
    /// "same name" or "same host+user".
    pub reason: &'static str,
}

/// An existing SSH node is a duplicate of `h` if it has the same name, or the
/// same host and (effective) user. Folder-scoped name matching would let the
/// same box in twice under different import folders, hence tree-wide.
fn find_duplicate<'a>(existing: &'a [(Node, Settings)], h: &ImportedHost) -> Option<(&'a Node, &'static str)> {
    if let Some((n, _)) = existing.iter().find(|(n, _)| n.name == h.name) {
        return Some((n, "same name"));
    }
    let host = h.host.to_ascii_lowercase();
    existing
        .iter()
        .find(|(_, eff)| eff.host.as_deref().map(|x| x.to_ascii_lowercase()) == Some(host.clone()) && eff.user == h.user)
        .map(|(n, _)| (n, "same host+user"))
}

/// Every SSH node with its effective settings, computed once per import.
fn existing_hosts(store: &Store) -> Result<Vec<(Node, Settings)>> {
    let all = store.all()?;
    let by_id: HashMap<&str, &Node> = all.iter().map(|n| (n.id.as_str(), n)).collect();
    Ok(all
        .iter()
        .filter(|n| n.kind == Kind::Ssh)
        .map(|n| {
            let mut chain = vec![n];
            let mut cur = n.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
            while let Some(p) = cur {
                chain.push(p);
                cur = p.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
            }
            let eff = chain.iter().rev().fold(Settings::default(), |acc, n| acc.merged_with(&n.settings));
            (n.clone(), eff)
        })
        .collect())
}

/// Dry run: nothing is written.
pub fn preview(store: &Store, parsed: &Parsed) -> Result<ImportPreview> {
    let existing = existing_hosts(store)?;
    let mut p = ImportPreview { warnings: parsed.warnings.clone(), ..Default::default() };
    for h in &parsed.hosts {
        match find_duplicate(&existing, h) {
            Some((n, reason)) => p.duplicates.push(Duplicate { name: h.name.clone(), host: h.host.clone(), existing: n.name.clone(), reason }),
            None => p.new.push(h.name.clone()),
        }
    }
    Ok(p)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    SshConfig,
    Putty,
    MobaXterm,
}

impl Source {
    pub fn parse_kind(kind: &str) -> Result<Self> {
        Ok(match kind {
            "ssh_config" => Self::SshConfig,
            "putty" => Self::Putty,
            "mobaxterm" => Self::MobaXterm,
            other => bail!("unknown import kind {other:?}"),
        })
    }

    /// Name of the folder imports land in.
    pub fn folder_name(self) -> &'static str {
        match self {
            Self::SshConfig => "ssh config",
            Self::Putty => "PuTTY",
            Self::MobaXterm => "MobaXterm",
        }
    }
}

/// Read and parse the source. `path` overrides each source's default location
/// (`~/.ssh/config`, the PuTTY registry hive / `~/.putty/sessions`, none for MobaXterm).
pub fn load(source: Source, path: Option<&Path>) -> Result<Parsed> {
    match source {
        Source::SshConfig => {
            let path = match path {
                Some(p) => p.to_path_buf(),
                None => ssh_config::default_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?,
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
            Ok(ssh_config::parse(&text))
        }
        Source::Putty => putty::load(path),
        Source::MobaXterm => {
            let path = path.ok_or_else(|| anyhow::anyhow!("a .mxtsessions file is required"))?;
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
            Ok(mobaxterm::parse(&text))
        }
    }
}

/// Write parsed hosts into the store under a folder named after the source.
/// Duplicates (see `find_duplicate`) are handled per `policy`; with the default
/// `Skip`, re-importing is safe.
pub fn apply(store: &Store, vault: &dyn Vault, source: Source, parsed: Parsed, policy: DupPolicy) -> Result<ImportReport> {
    let mut report = ImportReport { warnings: parsed.warnings, ..Default::default() };
    let root = ensure_folder(store, None, source.folder_name())?;
    let mut vault_warned = false;
    let existing = existing_hosts(store)?;

    for h in parsed.hosts {
        let dup = find_duplicate(&existing, &h).map(|(n, _)| n.clone());
        if dup.is_some() && policy == DupPolicy::Skip {
            report.skipped += 1;
            continue;
        }
        let mut parent = root.clone();
        for seg in &h.folder {
            parent = ensure_folder(store, Some(&parent), seg)?;
        }

        let credential = match &h.identity_file {
            Some(path) => match key_credential(vault, path) {
                Ok(name) => Some(name),
                Err(e) => {
                    if !vault_warned {
                        report.warnings.push(format!("identity files not saved to vault: {e}"));
                        vault_warned = true;
                    }
                    None
                }
            },
            None => None,
        };

        let imported = Settings {
            host: Some(h.host),
            port: h.port,
            user: h.user,
            credential,
            jump_host: h.jump_host,
            font: h.font,
            font_size: h.font_size,
            ..Default::default()
        };
        if let (Some(n), DupPolicy::Overwrite) = (dup, policy) {
            // Keep id, place, name, colour, tags…; imported fields win where the import has them.
            store.upsert(&Node { settings: n.settings.merged_with(&imported), ..n })?;
            report.updated += 1;
            continue;
        }
        let name = unique_child_name(store, &parent, &h.name)?;
        store.upsert(&Node {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id: Some(parent.clone()),
            kind: Kind::Ssh,
            name,
            sort: 0,
            settings: imported,
        })?;
        report.added += 1;
    }
    Ok(report)
}

/// `name`, or `name (2)`, `name (3)`… if that already exists under `parent`.
fn unique_child_name(store: &Store, parent: &str, name: &str) -> Result<String> {
    if store.find_child(Some(parent), name)?.is_none() {
        return Ok(name.to_string());
    }
    for i in 2.. {
        let candidate = format!("{name} ({i})");
        if store.find_child(Some(parent), &candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    unreachable!()
}

/// Find-or-create a folder by name under `parent`; returns its id.
fn ensure_folder(store: &Store, parent: Option<&str>, name: &str) -> Result<String> {
    if let Some(n) = store.find_child(parent, name)? {
        return Ok(n.id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    store.upsert(&Node {
        id: id.clone(),
        parent_id: parent.map(str::to_string),
        kind: Kind::Folder,
        name: name.to_string(),
        sort: 0,
        settings: Settings::default(),
    })?;
    Ok(id)
}

/// Vault entry for an identity file, named after the file. The same path maps
/// to the same entry; a different path with the same file name gets a suffix.
fn key_credential(vault: &dyn Vault, path: &str) -> Result<String> {
    let stem = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "key".into());
    for i in 0.. {
        let name = if i == 0 { stem.clone() } else { format!("{stem} ({i})") };
        match vault.get(&name)? {
            Some(s) if s.kind == SecretKind::Key && s.value == path => return Ok(name),
            Some(_) => continue,
            None => {
                vault.set(&name, &Secret { kind: SecretKind::Key, value: path.to_string(), key_passphrase: None })?;
                return Ok(name);
            }
        }
    }
    unreachable!()
}

/// `~/x` → home-relative absolute path; anything else unchanged.
pub(crate) fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

pub(crate) fn home_join(rel: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::FileVault;

    #[test]
    fn apply_creates_folder_skips_duplicates_and_links_keys() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::open(dir.path());
        vault.unlock("m").unwrap();

        let parsed = Parsed {
            hosts: vec![
                ImportedHost { name: "web".into(), host: "10.0.0.1".into(), user: Some("ops".into()), identity_file: Some("/k/id_ed25519".into()), ..Default::default() },
                ImportedHost { name: "db".into(), host: "10.0.0.2".into(), folder: vec!["prod".into(), "eu".into()], font: Some("Hack".into()), font_size: Some(15), ..Default::default() },
            ],
            warnings: vec!["w".into()],
        };
        let r = apply(&store, &vault, Source::SshConfig, parsed, DupPolicy::Skip).unwrap();
        assert_eq!((r.added, r.skipped), (2, 0));
        assert_eq!(r.warnings, vec!["w"]);

        let all = store.all().unwrap();
        let root = all.iter().find(|n| n.name == "ssh config" && n.kind == Kind::Folder).unwrap();
        let web = all.iter().find(|n| n.name == "web").unwrap();
        assert_eq!(web.parent_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(web.settings.credential.as_deref(), Some("id_ed25519"));
        assert_eq!(vault.get("id_ed25519").unwrap().unwrap().value, "/k/id_ed25519");
        let eu = all.iter().find(|n| n.name == "eu" && n.kind == Kind::Folder).unwrap();
        let db = all.iter().find(|n| n.name == "db").unwrap();
        assert_eq!(db.parent_id.as_deref(), Some(eu.id.as_str()));
        assert_eq!((db.settings.font.as_deref(), db.settings.font_size), (Some("Hack"), Some(15)));

        // Second import of the same thing: nothing added, folder reused.
        let again = || Parsed {
            hosts: vec![ImportedHost { name: "web".into(), host: "x".into(), user: Some("root".into()), ..Default::default() }],
            warnings: vec![],
        };
        let r = apply(&store, &vault, Source::SshConfig, again(), DupPolicy::Skip).unwrap();
        assert_eq!((r.added, r.skipped, r.updated), (0, 1, 0));
        assert_eq!(store.all().unwrap().iter().filter(|n| n.name == "ssh config").count(), 1);

        // Overwrite: same node id, imported fields replaced, the rest kept.
        let r = apply(&store, &vault, Source::SshConfig, again(), DupPolicy::Overwrite).unwrap();
        assert_eq!((r.added, r.skipped, r.updated), (0, 0, 1));
        let web2 = store.get(&web.id).unwrap().unwrap();
        assert_eq!((web2.settings.host.as_deref(), web2.settings.user.as_deref()), (Some("x"), Some("root")));
        assert_eq!(web2.settings.credential.as_deref(), Some("id_ed25519")); // not in the import → kept

        // Copy: added beside it with a suffixed name.
        let r = apply(&store, &vault, Source::SshConfig, again(), DupPolicy::Copy).unwrap();
        assert_eq!((r.added, r.skipped, r.updated), (1, 0, 0));
        assert!(store.all().unwrap().iter().any(|n| n.name == "web (2)"));
    }

    #[test]
    fn preview_flags_same_name_and_same_host_user_tree_wide() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store.upsert(&Node {
            id: "f".into(), parent_id: None, kind: Kind::Folder, name: "prod".into(), sort: 0,
            settings: Settings { user: Some("ops".into()), ..Default::default() },
        }).unwrap();
        store.upsert(&Node {
            id: "a".into(), parent_id: Some("f".into()), kind: Kind::Ssh, name: "alpha".into(), sort: 0,
            settings: Settings { host: Some("10.0.0.1".into()), ..Default::default() },
        }).unwrap();
        let parsed = Parsed {
            hosts: vec![
                ImportedHost { name: "alpha".into(), host: "1.1.1.1".into(), ..Default::default() },
                // user inherited from the folder counts
                ImportedHost { name: "other".into(), host: "10.0.0.1".into(), user: Some("ops".into()), ..Default::default() },
                ImportedHost { name: "fresh".into(), host: "10.0.0.1".into(), user: Some("someone-else".into()), ..Default::default() },
            ],
            warnings: vec![],
        };
        let p = preview(&store, &parsed).unwrap();
        assert_eq!(p.new, vec!["fresh"]);
        assert_eq!(
            p.duplicates,
            vec![
                Duplicate { name: "alpha".into(), host: "1.1.1.1".into(), existing: "alpha".into(), reason: "same name" },
                Duplicate { name: "other".into(), host: "10.0.0.1".into(), existing: "alpha".into(), reason: "same host+user" },
            ]
        );
        assert!(store.all().unwrap().iter().all(|n| n.name != "fresh")); // dry run
    }

    #[test]
    fn key_credential_disambiguates_same_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::open(dir.path());
        vault.unlock("m").unwrap();
        assert_eq!(key_credential(&vault, "/a/id_rsa").unwrap(), "id_rsa");
        assert_eq!(key_credential(&vault, "/a/id_rsa").unwrap(), "id_rsa");
        assert_eq!(key_credential(&vault, "/b/id_rsa").unwrap(), "id_rsa (1)");
    }

    #[test]
    fn locked_vault_warns_once_but_still_imports() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::open(dir.path()); // locked
        let parsed = Parsed {
            hosts: (0..2)
                .map(|i| ImportedHost { name: format!("h{i}"), host: "h".into(), identity_file: Some("/k".into()), ..Default::default() })
                .collect(),
            warnings: vec![],
        };
        let r = apply(&store, &vault, Source::Putty, parsed, DupPolicy::Skip).unwrap();
        assert_eq!(r.added, 2);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("locked"), "{:?}", r.warnings);
    }
}
