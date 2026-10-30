//! The tabs open at quit, so the next start can reopen them (`data/open-tabs.json`).
//!
//! Every window reports its tab list whenever it changes (`set`); the file always
//! holds the latest lists, so a crash loses nothing either. A tab window the user
//! closes takes its tabs out (`closed`); once the app is quitting (`quitting`),
//! windows going away no longer count as closing their tabs. The content is opaque
//! here (the UI's `SavedWindow`, see `src/lib/opentabs.ts`). Pure, no tauri types.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    windows: BTreeMap<String, Value>,
    quitting: bool,
}

pub struct OpenTabs {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl OpenTabs {
    /// Starts empty: the previous run's file is only read by `saved`, and the
    /// first `set` replaces it.
    pub fn new(data_dir: &Path) -> Self {
        OpenTabs { path: data_dir.join("open-tabs.json"), inner: Mutex::default() }
    }

    /// What the previous run left open, window label → that window's tabs.
    /// Missing or unreadable file = nothing.
    pub fn saved(&self) -> BTreeMap<String, Value> {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return BTreeMap::new() };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(mut o)) => match o.remove("windows") {
                Some(Value::Object(w)) => w.into_iter().collect(),
                _ => BTreeMap::new(),
            },
            _ => {
                tracing::warn!(path = %self.path.display(), "open tabs: unreadable, ignored");
                BTreeMap::new()
            }
        }
    }

    pub fn set(&self, window: &str, tabs: Value) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        if g.quitting {
            return Ok(()); // the windows are tearing down; keep what was open
        }
        g.windows.insert(window.to_string(), tabs);
        self.write(&g)
    }

    /// A window was closed. During quit this is ignored, so its tabs reopen.
    pub fn closed(&self, window: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        if g.quitting || g.windows.remove(window).is_none() {
            return Ok(());
        }
        self.write(&g)
    }

    /// The app is going away: freeze the file as it is.
    pub fn quitting(&self) {
        self.inner.lock().unwrap().quitting = true;
    }

    fn write(&self, g: &Inner) -> Result<()> {
        let body = serde_json::to_string_pretty(&serde_json::json!({ "windows": g.windows }))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("replace {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_every_window_through_quit_and_drops_closed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let t = OpenTabs::new(dir.path());
        assert!(t.saved().is_empty());

        t.set("main", json!({ "tabs": [1, 2] })).unwrap();
        t.set("tab-a", json!({ "tabs": [3] })).unwrap();
        t.set("tab-b", json!({ "tabs": [4] })).unwrap();
        // The user closes one tab window: its tabs are gone.
        t.closed("tab-b").unwrap();
        // Quit: the main window and the rest go away, nothing is dropped.
        t.quitting();
        t.closed("tab-a").unwrap();
        t.set("main", json!({ "tabs": [] })).unwrap();
        t.closed("main").unwrap();

        let next = OpenTabs::new(dir.path());
        let saved = next.saved();
        assert_eq!(saved.keys().collect::<Vec<_>>(), vec!["main", "tab-a"]);
        assert_eq!(saved["main"], json!({ "tabs": [1, 2] }));

        // The next run's first report replaces the old file.
        next.set("main", json!({ "tabs": [9] })).unwrap();
        assert_eq!(OpenTabs::new(dir.path()).saved().len(), 1);
    }

    #[test]
    fn unreadable_file_is_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("open-tabs.json"), "{not json").unwrap();
        assert!(OpenTabs::new(dir.path()).saved().is_empty());
    }
}
