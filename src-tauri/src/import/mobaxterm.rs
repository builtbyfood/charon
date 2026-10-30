//! MobaXterm `.mxtsessions` importer. INI-ish: each `[Bookmarks*]` section is a
//! folder (`SubRep=Parent\Child`) and each other key is a session:
//!
//! ```text
//! name=#<icon>#<type>%<host>%<port>%<user>%…%-1%…#<terminal settings>#…
//! ```
//!
//! Only type 0 (SSH) is imported. Beyond host/port/user the `%`-separated fields
//! are undocumented, so the private key is found by shape (a path to a key file)
//! rather than by position.

use super::{ImportedHost, Parsed};

pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let text = text.trim_start_matches('\u{feff}');
    let mut in_bookmarks = false;
    let mut folder: Vec<String> = Vec::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_bookmarks = section.starts_with("Bookmarks");
            folder.clear();
            continue;
        }
        if !in_bookmarks {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "SubRep" => {
                folder = value.split('\\').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
            }
            "ImgNum" => {}
            _ => match parse_session(key, value, &folder) {
                Ok(Some(h)) => out.hosts.push(h),
                Ok(None) => out.warnings.push(format!("{key}: skipped (not an SSH session)")),
                Err(e) => out.warnings.push(format!("line {}: {key}: {e}", lineno + 1)),
            },
        }
    }
    out
}

fn parse_session(name: &str, value: &str, folder: &[String]) -> Result<Option<ImportedHost>, String> {
    // "#109#0%host%22%user%…#font…#…" → the part between the 2nd '#' and the 3rd.
    let mut parts = value.splitn(4, '#');
    let (Some(""), Some(_icon), Some(body)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("not a session line".into());
    };
    let fields: Vec<&str> = body.split('%').collect();
    if fields.first() != Some(&"0") {
        return Ok(None);
    }
    let host = fields.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()).ok_or("no host")?;
    let port = fields.get(2).and_then(|p| p.trim().parse::<u16>().ok()).filter(|p| *p != 0);
    let user = fields.get(3).map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
    let identity_file = fields.iter().skip(4).map(|s| s.trim()).find(|s| looks_like_key_path(s)).map(str::to_string);
    Ok(Some(ImportedHost {
        name: clean_name(name, host, user.as_deref()),
        host: host.to_string(),
        port,
        user,
        identity_file,
        jump_host: None,
        folder: folder.to_vec(),
        ..Default::default()
    }))
}

/// MobaXterm auto-names sessions "label - host (user)". Drop the decorations
/// that only repeat the host/user fields; keep a bare host if that is all there is.
fn clean_name(name: &str, host: &str, user: Option<&str>) -> String {
    let mut n = name.trim();
    if let Some(u) = user {
        if let Some(stripped) = n.strip_suffix(&format!("({u})")) {
            n = stripped.trim_end();
        }
    }
    for sep in [" - ", " – ", " — "] {
        if let Some(stripped) = n.strip_suffix(&format!("{sep}{host}")) {
            n = stripped.trim_end();
        }
    }
    if n.is_empty() { host.to_string() } else { n.to_string() }
}

fn looks_like_key_path(s: &str) -> bool {
    if !(s.contains('/') || s.contains('\\')) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    lower.ends_with(".ppk")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.contains("id_rsa")
        || lower.contains("id_ed25519")
        || lower.contains("id_ecdsa")
        || lower.contains(".ssh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_auto_generated_names() {
        assert_eq!(clean_name("10.20.0.20 (admin)", "10.20.0.20", Some("admin")), "10.20.0.20");
        assert_eq!(clean_name("buildbox - 10.20.0.6 (travis)", "10.20.0.6", Some("travis")), "buildbox");
        assert_eq!(clean_name("buildbox - 10.20.0.6", "10.20.0.6", None), "buildbox");
        // Different user/host in the decoration is information: keep it.
        assert_eq!(clean_name("db (root)", "10.0.0.1", Some("dbadmin")), "db (root)");
        assert_eq!(clean_name("db - other.host", "10.0.0.1", None), "db - other.host");
        assert_eq!(clean_name("  (admin)", "h", Some("admin")), "h");
    }

    #[test]
    fn parses_ssh_sessions_and_folders() {
        let p = parse(include_str!("fixtures/sessions.mxtsessions"));
        let names: Vec<_> = p.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["web", "db", "bastion"]);

        let web = &p.hosts[0];
        assert_eq!((web.host.as_str(), web.port, web.user.as_deref()), ("web.example.com", Some(22), Some("ops")));
        assert!(web.folder.is_empty());
        assert_eq!(web.identity_file, None);

        let db = &p.hosts[1];
        assert_eq!(db.folder, vec!["Prod", "EU"]);
        assert_eq!(db.port, Some(2222));
        assert_eq!(db.identity_file.as_deref(), Some("C:\\keys\\db.pem"));

        // No user in the line → None, not "".
        assert_eq!(p.hosts[2].user, None);

        // RDP and unknown entries are reported, not imported.
        assert!(p.warnings.iter().any(|w| w.starts_with("rdp box")));
        assert!(p.warnings.iter().any(|w| w.contains("junk")));
    }
}
