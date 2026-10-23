//! Connection tree: folders and hosts with inheritance, persisted in SQLite.
//!
//! A folder may set any field a host can set; a host's *effective* config is the
//! walk from root to leaf with later values overriding earlier ones. That is what
//! makes "this whole folder goes through the prod bastion" a one-line change.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Folder,
    Ssh,
    Serial,
    Local,
    Rdp,
}

/// Fields that can be set on a folder or a host. Everything is optional so
/// folders can set a subset and hosts inherit the rest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    /// Reference into the credential vault, never the secret itself.
    pub credential: Option<String>,
    pub jump_host: Option<String>,
    /// Tab/gutter colour so you always know which box you're on.
    pub color: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Commands run after the shell is up (e.g. `sudo -i`).
    pub startup: Option<Vec<String>>,
    /// Inject OSC 133 shell integration. Default true.
    pub shell_integration: Option<bool>,
    /// MCP permission tier: "read", "exec-approve", "trusted". Default "read".
    pub mcp_tier: Option<String>,
    /// Allow MCP run_command to auto-answer this host's `[sudo] password for
    /// <login-user>:` prompt at the "trusted" tier. Default false.
    pub mcp_sudo: Option<bool>,
    /// Local sessions: program to run (default: platform shell) and start dir.
    pub shell: Option<String>,
    /// SSH sessions: shell to start instead of the account's login shell
    /// ("zsh", "/usr/bin/fish", "pwsh"); see `ssh::shell_command`.
    pub remote_shell: Option<String>,
    pub cwd: Option<String>,
    /// Serial sessions: port + line settings.
    pub serial: Option<crate::serial::SerialSettings>,
}

impl Settings {
    /// `other` overrides `self` wherever `other` has a value.
    pub fn merged_with(&self, other: &Settings) -> Settings {
        macro_rules! pick { ($f:ident) => { other.$f.clone().or_else(|| self.$f.clone()) }; }
        Settings {
            host: pick!(host),
            port: pick!(port),
            user: pick!(user),
            credential: pick!(credential),
            jump_host: pick!(jump_host),
            color: pick!(color),
            tags: pick!(tags),
            startup: pick!(startup),
            shell_integration: pick!(shell_integration),
            mcp_tier: pick!(mcp_tier),
            mcp_sudo: pick!(mcp_sudo),
            shell: pick!(shell),
            remote_shell: pick!(remote_shell),
            cwd: pick!(cwd),
            serial: pick!(serial),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub sort: i64,
    pub settings: Settings,
}

pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS nodes (
                id        TEXT PRIMARY KEY,
                parent_id TEXT REFERENCES nodes(id) ON DELETE CASCADE,
                kind      TEXT NOT NULL,
                name      TEXT NOT NULL,
                sort      INTEGER NOT NULL DEFAULT 0,
                settings  TEXT NOT NULL DEFAULT '{}'
            );
            CREATE TABLE IF NOT EXISTS history (
                id         INTEGER PRIMARY KEY,
                node_id    TEXT,
                command    TEXT NOT NULL,
                exit_code  INTEGER,
                ran_at     INTEGER NOT NULL
            );
            "#,
        )?;
        Ok(Self { db })
    }

    pub fn all(&self) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .prepare("SELECT id, parent_id, kind, name, sort, settings FROM nodes ORDER BY sort, name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Node {
                id: r.get(0)?,
                parent_id: r.get(1)?,
                kind: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(2)?)).unwrap_or(Kind::Ssh),
                name: r.get(3)?,
                sort: r.get(4)?,
                settings: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn upsert(&self, n: &Node) -> Result<()> {
        let kind = serde_json::to_string(&n.kind)?.trim_matches('"').to_string();
        self.db.execute(
            r#"INSERT INTO nodes (id, parent_id, kind, name, sort, settings)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)
               ON CONFLICT(id) DO UPDATE SET parent_id=excluded.parent_id, kind=excluded.kind,
                 name=excluded.name, sort=excluded.sort, settings=excluded.settings"#,
            params![n.id, n.parent_id, kind, n.name, n.sort, serde_json::to_string(&n.settings)?],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.execute("DELETE FROM nodes WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Node>> {
        Ok(self
            .db
            .query_row(
                "SELECT id, parent_id, kind, name, sort, settings FROM nodes WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Node {
                        id: r.get(0)?,
                        parent_id: r.get(1)?,
                        kind: serde_json::from_str(&format!("\"{}\"", r.get::<_, String>(2)?)).unwrap_or(Kind::Ssh),
                        name: r.get(3)?,
                        sort: r.get(4)?,
                        settings: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                    })
                },
            )
            .optional()?)
    }

    /// A credential was renamed in the vault: every node (host or folder) that
    /// referenced the old name follows. Returns how many changed.
    pub fn rename_credential(&self, from: &str, to: &str) -> Result<usize> {
        let mut n = 0;
        for mut node in self.all()? {
            if node.settings.credential.as_deref() == Some(from) {
                node.settings.credential = Some(to.to_string());
                self.upsert(&node)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Set (or clear, with None / blank) a node's own remote shell.
    pub fn set_remote_shell(&self, id: &str, shell: Option<&str>) -> Result<()> {
        let mut node = self.get(id)?.ok_or_else(|| anyhow::anyhow!("no connection {id}"))?;
        node.settings.remote_shell = shell.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        self.upsert(&node)
    }

    /// Direct child of `parent` (None = root) with this exact name, if any.
    pub fn find_child(&self, parent: Option<&str>, name: &str) -> Result<Option<Node>> {
        Ok(self.all()?.into_iter().find(|n| n.parent_id.as_deref() == parent && n.name == name))
    }

    /// Effective settings: root folder → … → this node.
    pub fn effective(&self, id: &str) -> Result<Settings> {
        let all: HashMap<String, Node> = self.all()?.into_iter().map(|n| (n.id.clone(), n)).collect();
        let mut chain = Vec::new();
        let mut cur = all.get(id);
        while let Some(n) = cur {
            chain.push(n);
            cur = n.parent_id.as_ref().and_then(|p| all.get(p));
        }
        Ok(chain
            .iter()
            .rev()
            .fold(Settings::default(), |acc, n| acc.merged_with(&n.settings)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_settings_inherit_and_override() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store.upsert(&Node {
            id: "prod".into(), parent_id: None, kind: Kind::Folder, name: "prod".into(), sort: 0,
            settings: Settings { jump_host: Some("bastion".into()), user: Some("ops".into()), ..Default::default() },
        }).unwrap();
        store.upsert(&Node {
            id: "web1".into(), parent_id: Some("prod".into()), kind: Kind::Ssh, name: "web1".into(), sort: 0,
            settings: Settings { host: Some("10.0.0.5".into()), user: Some("travis".into()), ..Default::default() },
        }).unwrap();
        let eff = store.effective("web1").unwrap();
        assert_eq!(eff.jump_host.as_deref(), Some("bastion"));
        assert_eq!(eff.user.as_deref(), Some("travis"));
        assert_eq!(eff.host.as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn renaming_a_credential_updates_every_node_that_uses_it() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let node = |id: &str, parent: Option<&str>, cred: Option<&str>| Node {
            id: id.into(), parent_id: parent.map(Into::into), kind: if parent.is_none() { Kind::Folder } else { Kind::Ssh },
            name: id.into(), sort: 0, settings: Settings { credential: cred.map(Into::into), ..Default::default() },
        };
        store.upsert(&node("f", None, Some("old"))).unwrap();
        store.upsert(&node("a", Some("f"), Some("old"))).unwrap();
        store.upsert(&node("b", Some("f"), Some("other"))).unwrap();
        store.upsert(&node("c", Some("f"), None)).unwrap();
        assert_eq!(store.rename_credential("old", "new").unwrap(), 2);
        assert_eq!(store.get("a").unwrap().unwrap().settings.credential.as_deref(), Some("new"));
        assert_eq!(store.get("f").unwrap().unwrap().settings.credential.as_deref(), Some("new"));
        assert_eq!(store.get("b").unwrap().unwrap().settings.credential.as_deref(), Some("other"));
        assert_eq!(store.effective("c").unwrap().credential.as_deref(), Some("new"));
    }

    #[test]
    fn remote_shell_is_set_cleared_and_inherited() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store.upsert(&Node { id: "f".into(), parent_id: None, kind: Kind::Folder, name: "f".into(), sort: 0,
            settings: Settings { remote_shell: Some("zsh".into()), ..Default::default() } }).unwrap();
        store.upsert(&Node { id: "h".into(), parent_id: Some("f".into()), kind: Kind::Ssh, name: "h".into(), sort: 0,
            settings: Default::default() }).unwrap();
        assert_eq!(store.effective("h").unwrap().remote_shell.as_deref(), Some("zsh"));
        store.set_remote_shell("h", Some(" fish ")).unwrap();
        assert_eq!(store.effective("h").unwrap().remote_shell.as_deref(), Some("fish"));
        store.set_remote_shell("h", Some("  ")).unwrap();
        assert_eq!(store.get("h").unwrap().unwrap().settings.remote_shell, None);
        assert_eq!(store.effective("h").unwrap().remote_shell.as_deref(), Some("zsh"));
    }
}
