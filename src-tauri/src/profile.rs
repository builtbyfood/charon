//! Profiles: appearance + behaviour, as TOML in `data/profiles/<name>.toml`.
//! Missing keys take defaults; unknown keys survive a load/save round-trip so
//! hand edits and future sections are not thrown away.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The profile shipped in the repo; copied into the data dir on first run.
pub const BUNDLED_DEFAULT: &str = include_str!("../../profiles/default.toml");

macro_rules! section {
    ($name:ident { $($(#[$m:meta])* $field:ident : $ty:ty = $default:expr),* $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $name {
            $($(#[$m])* pub $field: $ty,)*
            #[serde(flatten)]
            pub extra: toml::Table,
        }
        impl Default for $name {
            fn default() -> Self {
                Self { $($field: $default,)* extra: toml::Table::new() }
            }
        }
    };
}

section!(Appearance {
    font: String = "JetBrains Mono".into(),
    font_size: u16 = 13,
    theme: String = "charon-dark".into(),
    scrollback: u32 = 50_000,
    /// "webgl" or "dom".
    renderer: String = "webgl".into(),
});

section!(TerminalPrefs {
    /// Gutter with the time each line was printed (status-bar switch).
    timestamps: bool = false,
    copy_on_select: bool = false,
    ctrl_c_copies_when_selected: bool = true,
    middle_click_paste: bool = true,
    bracketed_paste: bool = true,
    paste_guard_lines: u32 = 2,
    /// When the remote has bracketed paste on and the text ends in a newline:
    /// "auto" runs it (newline sent after the 201~ end marker), "wait" leaves it
    /// highlighted for the user to press Enter.
    paste_run: String = "auto".into(),
});

section!(Clipboard {
    strip_zero_width: bool = true,
    nbsp_to_space: bool = true,
    smart_quotes_to_ascii: bool = true,
    trim_trailing_whitespace: bool = true,
    crlf_to_lf: bool = true,
});

section!(Commands {
    mark_gutter: bool = true,
    tint_failed_commands: bool = true,
    jump_keys: Vec<String> = vec!["Ctrl+Up".into(), "Ctrl+Down".into()],
});

section!(Ui {
    sidebar_width: u32 = 260,
});

section!(Ssh {
    /// Server-alive interval in seconds; 0 disables keepalive. Idle sessions are
    /// no longer dropped by an inactivity timeout.
    keepalive_seconds: u64 = 30,
});

section!(Mcp {
    enabled: bool = false,
    /// TCP port for the HTTP transport; 0 = pick a free one.
    port: u16 = 0,
    /// Tier for sessions with no saved node: "read" | "exec-approve" | "trusted".
    default_tier: String = "read".into(),
});

section!(Files {
    /// The file pane opens with every SSH / local tab (Ctrl+Shift+E toggles).
    auto_open: bool = true,
    /// Max files transferring at once during a recursive/multi-item transfer.
    parallel_transfers: u16 = 4,
});

section!(Sudo {
    /// Send the session's stored password automatically (after a 300 ms debounce)
    /// when a sudo/su prompt is detected; false = show a button / wait for Ctrl+Shift+P.
    auto_send: bool = false,
});

section!(Local {
    /// Vault entry name whose password the bare "Local" shell (no saved node)
    /// offers to a sudo/su prompt (Unix). Empty = none. Saved local nodes use
    /// their own credential instead.
    credential: String = "".into(),
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub appearance: Appearance,
    pub terminal: TerminalPrefs,
    pub clipboard: Clipboard,
    pub commands: Commands,
    pub ui: Ui,
    #[serde(default)]
    pub ssh: Ssh,
    #[serde(default)]
    pub mcp: Mcp,
    #[serde(default)]
    pub files: Files,
    #[serde(default)]
    pub sudo: Sudo,
    #[serde(default)]
    pub local: Local,
    /// Sections this build does not model yet (docs, …) pass through untouched.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "default".into(),
            appearance: Default::default(),
            terminal: Default::default(),
            clipboard: Default::default(),
            commands: Default::default(),
            ui: Default::default(),
            ssh: Default::default(),
            mcp: Default::default(),
            files: Default::default(),
            sudo: Default::default(),
            local: Default::default(),
            extra: toml::Table::new(),
        }
    }
}

impl Profile {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("parse profile")
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serialise profile")
    }
}

pub fn path_for(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join("profiles").join(format!("{name}.toml"))
}

/// Load `data/profiles/default.toml`, seeding it from the bundled copy on first
/// run. A profile that fails to parse is logged and replaced by defaults in
/// memory (never on disk) so a typo cannot keep the app from starting.
pub fn load_default(data_dir: &Path) -> Profile {
    let path = path_for(data_dir, "default");
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, BUNDLED_DEFAULT) {
            tracing::warn!(path = %path.display(), "could not seed default profile: {e}");
        }
    }
    match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|t| Profile::parse(&t)) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(path = %path.display(), "profile unreadable, using defaults: {e:#}");
            Profile::default()
        }
    }
}

pub fn save(data_dir: &Path, profile: &Profile) -> Result<()> {
    let path = path_for(data_dir, &profile.name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, profile.to_toml()?).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_default_parses_and_matches_code_defaults() {
        let p = Profile::parse(BUNDLED_DEFAULT).unwrap();
        assert_eq!(p.name, "default");
        assert_eq!(p.appearance.font_size, 13);
        assert_eq!(p.appearance.scrollback, 50_000);
        assert!(!p.terminal.copy_on_select);
        assert_eq!(p.commands.jump_keys, vec!["Ctrl+Up", "Ctrl+Down"]);
        // mcp / ssh are modelled now; their sections parse.
        assert!(!p.mcp.enabled);
        assert_eq!(p.mcp.default_tier, "read");
        assert_eq!(p.ssh.keepalive_seconds, 30);
        // Sections not modelled here still ride along.
        assert!(p.extra.contains_key("docs"));
    }

    #[test]
    fn missing_keys_default_and_unknown_keys_round_trip() {
        let p = Profile::parse("name = \"work\"\n[appearance]\nfont_size = 15\ncustom_thing = true\n").unwrap();
        assert_eq!(p.name, "work");
        assert_eq!(p.appearance.font_size, 15);
        assert_eq!(p.appearance.font, "JetBrains Mono");
        assert_eq!(p.ui.sidebar_width, 260);
        let text = p.to_toml().unwrap();
        assert!(text.contains("custom_thing = true"), "{text}");
        assert_eq!(Profile::parse(&text).unwrap(), p);
    }

    #[test]
    fn load_seeds_then_save_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = load_default(dir.path());
        assert!(path_for(dir.path(), "default").exists(), "seeded from bundled copy");
        p.appearance.font_size = 17;
        p.ui.sidebar_width = 333;
        save(dir.path(), &p).unwrap();
        let again = load_default(dir.path());
        assert_eq!(again.appearance.font_size, 17);
        assert_eq!(again.ui.sidebar_width, 333);
        assert!(again.extra.contains_key("docs"), "unknown sections kept across save");
    }

    #[test]
    fn broken_profile_falls_back_to_defaults_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(dir.path(), "default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not = [toml").unwrap();
        let p = load_default(dir.path());
        assert_eq!(p, Profile::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is not = [toml");
    }
}
