//! `~/.ssh/config` importer: Host, HostName, User, Port, IdentityFile, ProxyJump.
//! Wildcard `Host` patterns and `Match`/`Include` blocks are skipped with a warning;
//! a wildcard block's values are not applied as defaults (ssh's first-wins merge
//! semantics are more than this needs).

use super::{expand_tilde, home_join, ImportedHost, Parsed};
use std::path::PathBuf;

pub fn default_path() -> Option<PathBuf> {
    home_join(".ssh/config")
}

pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    // Aliases of the current block; empty while inside a skipped block.
    let mut current: Vec<ImportedHost> = Vec::new();
    let mut skipping = false;
    let mut warned_wildcard = false;

    let flush = |current: &mut Vec<ImportedHost>, out: &mut Parsed| {
        for mut h in current.drain(..) {
            if h.host.is_empty() {
                h.host = h.name.clone(); // no HostName: the alias is the address
            }
            out.hosts.push(h);
        }
    };

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        // "Key value", "Key=value" and "Key = value" are all valid.
        let (key, value) = match line.split_once(|c: char| c == ' ' || c == '\t' || c == '=') {
            Some((k, v)) => (k.trim(), v.trim_start_matches(|c: char| c == ' ' || c == '\t' || c == '=').trim()),
            None => (line, ""),
        };
        let value = value.trim_matches('"');

        match key.to_ascii_lowercase().as_str() {
            "host" => {
                flush(&mut current, &mut out);
                skipping = false;
                for alias in value.split_whitespace() {
                    if alias.contains(['*', '?']) || alias.starts_with('!') {
                        if !warned_wildcard {
                            out.warnings.push(format!("line {}: wildcard Host patterns skipped", lineno + 1));
                            warned_wildcard = true;
                        }
                        continue;
                    }
                    current.push(ImportedHost { name: alias.to_string(), ..Default::default() });
                }
                if current.is_empty() {
                    skipping = true;
                }
            }
            "match" => {
                flush(&mut current, &mut out);
                skipping = true;
                out.warnings.push(format!("line {}: Match block skipped", lineno + 1));
            }
            "include" => out.warnings.push(format!("line {}: Include {value} not followed", lineno + 1)),
            _ if skipping || current.is_empty() => {}
            "hostname" => current.iter_mut().for_each(|h| h.host = value.to_string()),
            "user" => current.iter_mut().for_each(|h| h.user = Some(value.to_string())),
            "port" => match value.parse::<u16>() {
                Ok(p) => current.iter_mut().for_each(|h| h.port = Some(p)),
                Err(_) => out.warnings.push(format!("line {}: bad port {value:?}", lineno + 1)),
            },
            "identityfile" => {
                // First IdentityFile wins, like ssh's own precedence.
                let p = expand_tilde(value);
                current.iter_mut().for_each(|h| { h.identity_file.get_or_insert_with(|| p.clone()); });
            }
            "proxyjump" => current.iter_mut().for_each(|h| h.jump_host = Some(value.to_string())),
            _ => {}
        }
    }
    flush(&mut current, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/ssh_config");

    #[test]
    fn parses_hosts_and_skips_wildcards() {
        let p = parse(FIXTURE);
        let names: Vec<_> = p.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["web", "web-alt", "bastion", "db", "plain"]);

        let web = &p.hosts[0];
        assert_eq!(web.host, "10.0.0.5");
        assert_eq!(web.user.as_deref(), Some("ops"));
        assert_eq!(web.port, Some(2222));
        assert!(web.identity_file.as_deref().unwrap().ends_with("id_ed25519"));
        assert!(!web.identity_file.as_deref().unwrap().starts_with('~'), "tilde expanded");
        assert_eq!(web.jump_host.as_deref(), Some("ops@bastion.example.com"));
        // Alias on the same Host line shares the block.
        assert_eq!(p.hosts[1].host, "10.0.0.5");

        // `Port=22` form and quoted value.
        let db = &p.hosts[3];
        assert_eq!(db.port, Some(22));
        assert_eq!(db.user.as_deref(), Some("db admin"));
        // First IdentityFile wins.
        assert_eq!(db.identity_file.as_deref(), Some("/keys/first"));

        // No HostName → alias is the address.
        assert_eq!(p.hosts[4].host, "plain");

        assert!(p.warnings.iter().any(|w| w.contains("wildcard")));
        assert!(p.warnings.iter().any(|w| w.contains("Match")));
        assert!(p.warnings.iter().any(|w| w.contains("Include")));
        // Values inside the wildcard/Match blocks never leak into real hosts.
        assert!(p.hosts.iter().all(|h| h.user.as_deref() != Some("nobody")));
    }
}
