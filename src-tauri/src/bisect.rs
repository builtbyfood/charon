//! `CHARON_BISECT`: switch parts of Charon's startup off, one at a time, to find
//! what stops WebKitGTK presenting frames without input on some Linux boxes (a
//! bare create-tauri-app on the same machine updates live). Comma-separated:
//!
//! - `dmabuf`: leave WebKitGTK's DMABUF renderer on even on a VM (`webkitenv.rs`;
//!   this flag found the stall: forcing it off was the cause)
//! - `noplugins`: skip the shell / dialog / clipboard-manager plugins
//! - `nofocus`: skip the webview `set_focus` on page load
//! - `nomcp`: skip the MCP server autostart
//! - `nowake`: skip the glib main-context wakeup after output
//! - `norepaint`: don't start the repaint thread (`repaint.rs`)
//! - `page=bare|bare-ipc|css`: the page (`src/main.ts`): only a ticker, no CSS or
//!   IPC / plus the IPC the app's ticker makes / plus the app's CSS and theme
//! - `noraf`, `noconsole`: the full page without the rAF fallback / the console
//!   forwarding
//!
//! The page reads the list from `window.__CHARON_BISECT__`, set by an init script
//! before any page script runs (no IPC needed). Unset = normal startup.

use std::collections::BTreeSet;
use std::sync::LazyLock;

pub fn parse(s: &str) -> BTreeSet<String> {
    s.split(',').map(|f| f.trim().to_ascii_lowercase()).filter(|f| !f.is_empty()).collect()
}

static FLAGS: LazyLock<BTreeSet<String>> =
    LazyLock::new(|| std::env::var("CHARON_BISECT").map(|s| parse(&s)).unwrap_or_default());

pub fn active() -> bool {
    !FLAGS.is_empty()
}

pub fn has(flag: &str) -> bool {
    FLAGS.contains(flag)
}

/// Logged at start so each run's log says what it tested.
pub fn describe() -> String {
    FLAGS.iter().cloned().collect::<Vec<_>>().join(",")
}

/// `window.__CHARON_BISECT__ = [...]` for the page.
pub fn init_script() -> String {
    format!("window.__CHARON_BISECT__ = {};", serde_json::to_string(&*FLAGS).unwrap_or_else(|_| "[]".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_comma_list() {
        let f = parse(" DMABUF, page=bare ,,nofocus");
        assert_eq!(f.into_iter().collect::<Vec<_>>(), vec!["dmabuf", "nofocus", "page=bare"]);
        assert!(parse("").is_empty());
    }
}
