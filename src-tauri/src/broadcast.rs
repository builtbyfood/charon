//! MultiExec roles as each window reports them (the broadcast state itself lives
//! in the UI and is shared between windows there), so MCP `list_sessions` can say
//! which sessions take part: "source", "target" or "off".

use std::collections::HashMap;
use std::sync::Mutex;

/// Window label → (session id → role).
#[derive(Default)]
pub struct Roles(Mutex<HashMap<String, HashMap<String, &'static str>>>);

impl Roles {
    /// Replace `window`'s roles; anything but "source" / "target" is dropped.
    pub fn set(&self, window: &str, roles: HashMap<String, String>) {
        let kept: HashMap<String, &'static str> = roles
            .into_iter()
            .filter_map(|(s, r)| match r.as_str() {
                "source" => Some((s, "source")),
                "target" => Some((s, "target")),
                _ => None,
            })
            .collect();
        let mut m = self.0.lock().unwrap();
        if kept.is_empty() {
            m.remove(window);
        } else {
            m.insert(window.to_string(), kept);
        }
    }

    /// A window went away: its tabs no longer take part.
    pub fn forget(&self, window: &str) {
        self.0.lock().unwrap().remove(window);
    }

    /// "source" if any window says so, else "target", else "off".
    pub fn role(&self, session: &str) -> &'static str {
        let m = self.0.lock().unwrap();
        let mut found = "off";
        for w in m.values() {
            match w.get(session) {
                Some(&"source") => return "source",
                Some(r) => found = r,
                None => {}
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(p: &[(&str, &str)]) -> HashMap<String, String> {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn roles_per_window_merge_and_clear() {
        let r = Roles::default();
        assert_eq!(r.role("a"), "off");
        r.set(
            "main",
            map(&[("a", "source"), ("b", "target"), ("x", "bogus")]),
        );
        r.set("tab-1", map(&[("c", "target")]));
        assert_eq!(
            (r.role("a"), r.role("b"), r.role("c"), r.role("x")),
            ("source", "target", "target", "off")
        );
        // The source moved to another window: each window reports its own view.
        r.set("main", map(&[("a", "target"), ("b", "target")]));
        r.set("tab-1", map(&[("c", "source")]));
        assert_eq!((r.role("a"), r.role("c")), ("target", "source"));
        // Broadcast off: windows report nothing.
        r.set("main", map(&[]));
        assert_eq!(r.role("a"), "off");
        r.forget("tab-1");
        assert_eq!(r.role("c"), "off");
    }
}
