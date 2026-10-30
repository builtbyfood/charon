//! Installed monospace font families, for the font pickers (Settings → Appearance,
//! the connection editor). Linux asks fontconfig; elsewhere the UI probes a
//! curated list itself (`src/lib/fonts.ts`), so this returns nothing there.

/// Families from `fc-list :spacing=mono family` output: one line per font face,
/// comma-separated aliases (first is the family's own name), deduplicated + sorted.
pub fn parse_fc_list(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out
        .lines()
        .filter_map(|l| l.split(',').next())
        .map(|f| f.replace("\\-", "-").trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    v.sort_by_key(|f| f.to_lowercase());
    v.dedup();
    v
}

pub fn mono_families() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        // spacing=mono (100) plus dual-width (90): CJK-capable terminal fonts are "dual".
        let mut all = Vec::new();
        for pattern in [":spacing=mono", ":spacing=dual"] {
            if let Ok(o) = std::process::Command::new("fc-list").args([pattern, "family"]).output() {
                all.extend(parse_fc_list(&String::from_utf8_lossy(&o.stdout)));
            }
        }
        all.sort_by_key(|f| f.to_lowercase());
        all.dedup();
        all
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fc_list_lines_become_unique_families() {
        let out = "DejaVu Sans Mono\nDejaVu Sans Mono,DejaVu Sans Mono Bold\nJetBrains Mono,JetBrains Mono ExtraBold\n\nNoto Sans Mono\\-CJK\nadwaita mono\n";
        assert_eq!(parse_fc_list(out), vec!["adwaita mono", "DejaVu Sans Mono", "JetBrains Mono", "Noto Sans Mono-CJK"]);
    }
}
