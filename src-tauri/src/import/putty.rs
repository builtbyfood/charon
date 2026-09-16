//! PuTTY sessions. Windows keeps them in the registry under
//! `HKCU\Software\SimonTatham\PuTTY\Sessions\<name>`; Linux PuTTY writes one
//! `Key=Value` file per session in `~/.putty/sessions/`. Both are reduced to the
//! same (name, key/value list) shape before parsing, so the parser is testable
//! from the file form on any platform.

use super::{ImportedHost, Parsed};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// One raw session: URL-encoded name plus its settings as strings.
pub struct RawSession {
    pub name: String,
    pub values: Vec<(String, String)>,
}

pub fn default_dir() -> Option<PathBuf> {
    super::home_join(".putty/sessions")
}

/// PuTTY escapes anything outside `[A-Za-z0-9]` in session names as `%XX`.
pub fn decode_name(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_sessions(sessions: Vec<RawSession>) -> Parsed {
    let mut out = Parsed::default();
    for s in sessions {
        let name = decode_name(&s.name);
        if name == "Default Settings" {
            continue;
        }
        let get = |k: &str| s.values.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());

        let proto = get("Protocol").unwrap_or("ssh");
        if proto != "ssh" {
            out.warnings.push(format!("{name}: skipped ({proto} session)"));
            continue;
        }
        let host_field = get("HostName").unwrap_or("").trim();
        if host_field.is_empty() {
            out.warnings.push(format!("{name}: skipped (no host)"));
            continue;
        }
        // PuTTY allows "user@host" in the host box.
        let (mut user, host) = match host_field.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h.to_string()),
            None => (None, host_field.to_string()),
        };
        if let Some(u) = get("UserName").map(str::trim).filter(|u| !u.is_empty()) {
            user = Some(u.to_string());
        }
        let port = get("PortNumber").and_then(|p| p.trim().parse::<u16>().ok()).filter(|p| *p != 0);
        let identity_file = get("PublicKeyFile").map(str::trim).filter(|p| !p.is_empty()).map(str::to_string);
        if identity_file.as_deref().is_some_and(|p| p.to_ascii_lowercase().ends_with(".ppk")) {
            out.warnings.push(format!("{name}: uses a .ppk key; if it has a passphrase, add it to the vault entry"));
        }
        out.hosts.push(ImportedHost {
            name,
            host,
            port,
            user,
            identity_file,
            jump_host: None,
            folder: vec![],
        });
    }
    out
}

/// Linux form: each file in the sessions directory is `Key=Value` lines.
pub fn parse_session_file(file_name: &str, text: &str) -> RawSession {
    let values = text
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    RawSession { name: file_name.to_string(), values }
}

pub fn load_dir(dir: &Path) -> Result<Parsed> {
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("read {}: {e}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(entry.path())?;
        sessions.push(parse_session_file(&entry.file_name().to_string_lossy(), &text));
    }
    Ok(parse_sessions(sessions))
}

#[cfg(windows)]
pub fn load_registry() -> Result<Parsed> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let root = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\SimonTatham\PuTTY\Sessions")
        .map_err(|e| anyhow::anyhow!("no PuTTY sessions in the registry: {e}"))?;
    let mut sessions = Vec::new();
    for name in root.enum_keys().flatten() {
        let key = root.open_subkey(&name)?;
        let mut values = Vec::new();
        for (vname, value) in key.enum_values().flatten() {
            let as_str = match value.vtype {
                winreg::enums::RegType::REG_DWORD => {
                    u32::from_le_bytes(value.bytes[..4].try_into().unwrap_or([0; 4])).to_string()
                }
                _ => value.to_string(),
            };
            values.push((vname, as_str));
        }
        sessions.push(RawSession { name, values });
    }
    Ok(parse_sessions(sessions))
}

/// `path` = a sessions directory (Linux layout). Without it: registry on Windows,
/// `~/.putty/sessions` elsewhere.
pub fn load(path: Option<&Path>) -> Result<Parsed> {
    if let Some(p) = path {
        return load_dir(p);
    }
    #[cfg(windows)]
    {
        load_registry()
    }
    #[cfg(not(windows))]
    {
        let dir = default_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        load_dir(&dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_putty_names() {
        assert_eq!(decode_name("My%20Server%20%2801%29"), "My Server (01)");
        assert_eq!(decode_name("plain"), "plain");
        assert_eq!(decode_name("bad%2"), "bad%2");
    }

    #[test]
    fn parses_session_files() {
        let files = [
            ("web%20prod", include_str!("fixtures/putty/web%20prod")),
            ("Default%20Settings", include_str!("fixtures/putty/Default%20Settings")),
            ("serial%20console", include_str!("fixtures/putty/serial%20console")),
            ("keyed", include_str!("fixtures/putty/keyed")),
        ];
        let p = parse_sessions(files.iter().map(|(n, t)| parse_session_file(n, t)).collect());
        let names: Vec<_> = p.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["web prod", "keyed"]);

        let web = &p.hosts[0];
        assert_eq!(web.host, "web.example.com");
        assert_eq!(web.port, Some(2200));
        assert_eq!(web.user.as_deref(), Some("ops"));
        assert_eq!(web.identity_file, None);

        // user@host in the host box, UserName empty, key file set.
        let keyed = &p.hosts[1];
        assert_eq!(keyed.host, "10.1.1.1");
        assert_eq!(keyed.user.as_deref(), Some("root"));
        assert_eq!(keyed.identity_file.as_deref(), Some("C:\\keys\\lab.ppk"));
        assert!(p.warnings.iter().any(|w| w.contains(".ppk")));
        assert!(p.warnings.iter().any(|w| w.contains("serial session")));
    }
}
