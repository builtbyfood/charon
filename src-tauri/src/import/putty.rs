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
        let (font, font_size) = match parse_font(get("Font"), get("FontHeight")) {
            FontSetting::Use(f, px) => (Some(f), px),
            FontSetting::Default => (None, None),
            FontSetting::Unusable(f) => {
                out.warnings.push(format!("{name}: font \"{f}\" is an X11 server font; the default terminal font is used"));
                (None, None)
            }
        };
        out.hosts.push(ImportedHost {
            name,
            host,
            port,
            user,
            identity_file,
            jump_host: None,
            folder: vec![],
            font,
            font_size,
        });
    }
    out
}

#[derive(Debug, PartialEq)]
enum FontSetting {
    /// Family plus size in px, if PuTTY gave one.
    Use(String, Option<u16>),
    /// Not set, or PuTTY's factory default: leave the connection on Charon's default.
    Default,
    /// An X11 core font (`server:fixed`), which a webview cannot use.
    Unusable(String),
}

/// PuTTY's font. Windows: `Font` is the family and `FontHeight` the size in
/// points. Unix PuTTY: `Font` is `client:<Pango description>` (family then
/// points, e.g. `client:DejaVu Sans Mono 11`) or `server:<X11 name>`. Points
/// become px at 96 dpi; PuTTY's out-of-the-box Courier New 10 is not carried
/// over, so untouched sessions follow Charon's default font.
fn parse_font(font: Option<&str>, height: Option<&str>) -> FontSetting {
    let raw = font.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return FontSetting::Default;
    }
    if let Some(x11) = raw.strip_prefix("server:") {
        return FontSetting::Unusable(x11.to_string());
    }
    let mut family = raw.strip_prefix("client:").unwrap_or(raw).trim().to_string();
    let mut points = height.and_then(|h| h.trim().parse::<f32>().ok()).filter(|p| *p > 0.0);
    if raw.starts_with("client:") {
        // Pango: "Family [Style…] Size"; the size is the last word when numeric.
        if let Some((rest, last)) = family.rsplit_once(' ') {
            if let Ok(p) = last.parse::<f32>() {
                points = Some(p);
                family = rest.trim().to_string();
            }
        }
        for style in [" Bold", " Italic", " Oblique", " Regular", " Medium", " Light"] {
            while let Some(f) = family.strip_suffix(style) {
                family = f.to_string();
            }
        }
    }
    if family.is_empty() {
        return FontSetting::Default;
    }
    let px = points.map(|p| ((p * 96.0 / 72.0).round() as u16).clamp(6, 40));
    if family.eq_ignore_ascii_case("Courier New") && points.is_none_or(|p| p == 10.0) {
        return FontSetting::Default;
    }
    FontSetting::Use(family, px)
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
            ("x11", include_str!("fixtures/putty/x11")),
        ];
        let p = parse_sessions(files.iter().map(|(n, t)| parse_session_file(n, t)).collect());
        let names: Vec<_> = p.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["web prod", "keyed", "x11"]);
        assert_eq!(p.hosts[2].font, None);
        assert!(p.warnings.iter().any(|w| w.starts_with("x11: font \"fixed\"")));

        let web = &p.hosts[0];
        assert_eq!(web.host, "web.example.com");
        assert_eq!(web.port, Some(2200));
        assert_eq!(web.user.as_deref(), Some("ops"));
        assert_eq!(web.identity_file, None);
        assert_eq!(web.font.as_deref(), Some("Consolas"));
        assert_eq!(web.font_size, Some(15));

        // user@host in the host box, UserName empty, key file set.
        let keyed = &p.hosts[1];
        assert_eq!(keyed.host, "10.1.1.1");
        assert_eq!(keyed.user.as_deref(), Some("root"));
        assert_eq!(keyed.identity_file.as_deref(), Some("C:\\keys\\lab.ppk"));
        assert_eq!(keyed.font, None, "Courier New 10 is PuTTY's default, not a choice");
        assert!(p.warnings.iter().any(|w| w.contains(".ppk")));
        assert!(p.warnings.iter().any(|w| w.contains("serial session")));
    }

    #[test]
    fn reads_fonts() {
        use FontSetting::*;
        // Windows registry: family + points.
        assert_eq!(parse_font(Some("Consolas"), Some("12")), Use("Consolas".into(), Some(16)));
        assert_eq!(parse_font(Some("Lucida Console"), None), Use("Lucida Console".into(), None));
        // PuTTY's factory default is left to Charon's default.
        assert_eq!(parse_font(Some("Courier New"), Some("10")), Default);
        assert_eq!(parse_font(Some("Courier New"), Some("14")), Use("Courier New".into(), Some(19)));
        assert_eq!(parse_font(None, Some("10")), Default);
        // Unix PuTTY: Pango description, size inside the string.
        assert_eq!(parse_font(Some("client:DejaVu Sans Mono 11"), None), Use("DejaVu Sans Mono".into(), Some(15)));
        assert_eq!(parse_font(Some("client:Fira Code Bold 10"), None), Use("Fira Code".into(), Some(13)));
        assert_eq!(parse_font(Some("client:Monospace"), None), Use("Monospace".into(), None));
        assert_eq!(parse_font(Some("server:fixed"), None), Unusable("fixed".into()));
        // Absurd sizes are clamped to what the UI accepts.
        assert_eq!(parse_font(Some("Hack"), Some("72")), Use("Hack".into(), Some(40)));
    }
}
