//! Moving a terminal tab between windows without dropping its connection.
//!
//! The session stays in the backend; only the xterm view moves. The source window
//! asks for a *hold* on the session (output is buffered here instead of emitted),
//! serialises its screen and stashes it under a token; the target window takes the
//! stash, restores the screen, starts listening and *releases* the hold, which
//! emits everything buffered in between — so no byte is lost or shown twice.
//! Pure (no tauri types) so it is unit-testable.

use std::collections::HashMap;
use std::sync::Mutex;

/// Cap on output buffered for a held session (a flood while a tab is in flight).
pub const HOLD_CAP: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct Held {
    buf: Vec<u8>,
    /// The session closed while held: delivered after the buffered bytes.
    closed: Option<String>,
}

/// Sessions whose output is parked while their tab moves between windows.
#[derive(Default)]
pub struct Holds(Mutex<HashMap<String, Held>>);

/// What a held session's output turned into: emit it now, or keep it.
pub enum Route<'a> {
    Emit(&'a [u8]),
    Held,
}

impl Holds {
    /// Park `id`'s output. `marker` runs under the lock, so it is ordered after
    /// every byte already emitted and before anything buffered.
    pub fn hold(&self, id: &str, marker: impl FnOnce()) {
        let mut m = self.0.lock().unwrap();
        m.entry(id.to_string()).or_default();
        marker();
    }

    /// Send `data` (via `emit`, under the lock so ordering with hold/release holds)
    /// unless the session is held, in which case it is buffered.
    pub fn data(&self, id: &str, data: &[u8], emit: impl FnOnce(&[u8])) {
        let mut m = self.0.lock().unwrap();
        match m.get_mut(id) {
            Some(h) => {
                h.buf.extend_from_slice(data);
                if h.buf.len() > HOLD_CAP {
                    let cut = h.buf.len() - HOLD_CAP;
                    h.buf.drain(..cut);
                }
            }
            None => emit(data),
        }
    }

    /// The session closed: tell the UI now, or after the held bytes on release.
    pub fn closed(&self, id: &str, reason: &str, emit: impl FnOnce(&str)) {
        let mut m = self.0.lock().unwrap();
        match m.get_mut(id) {
            Some(h) => h.closed = Some(reason.to_string()),
            None => emit(reason),
        }
    }

    /// Resume `id`: hand the buffered bytes (and a pending close) to the callbacks.
    pub fn release(&self, id: &str, emit: impl FnOnce(&[u8]), close: impl FnOnce(&str)) {
        let mut m = self.0.lock().unwrap();
        if let Some(h) = m.remove(id) {
            if !h.buf.is_empty() {
                emit(&h.buf);
            }
            if let Some(r) = h.closed {
                close(&r);
            }
        }
    }

    pub fn is_held(&self, id: &str) -> bool {
        self.0.lock().unwrap().contains_key(id)
    }
}

/// Serialised tabs in flight, keyed by a one-time token.
#[derive(Default)]
pub struct Stash(Mutex<HashMap<String, String>>);

impl Stash {
    pub fn put(&self, json: String) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.0.lock().unwrap().insert(token.clone(), json);
        token
    }
    pub fn take(&self, token: &str) -> Option<String> {
        self.0.lock().unwrap().remove(token)
    }
}

/// A window's outer rectangle in physical pixels.
#[derive(Debug, Clone, Copy)]
pub struct Rect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }

impl Rect {
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x as f64 && py >= self.y as f64 && px < self.x as f64 + self.w as f64 && py < self.y as f64 + self.h as f64
    }
}

/// The window under the cursor, skipping `exclude` (the window the drag started in).
/// The main window wins when windows overlap: it is where tabs usually go back to.
pub fn window_at<'a>(wins: &'a [(String, Rect)], exclude: &str, px: f64, py: f64) -> Option<&'a str> {
    let mut hit: Vec<&str> = wins.iter().filter(|(l, r)| l != exclude && r.contains(px, py)).map(|(l, _)| l.as_str()).collect();
    hit.sort_by_key(|l| *l != "main");
    hit.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn held_output_is_buffered_and_released_in_order() {
        let h = Holds::default();
        let out = RefCell::new(Vec::<u8>::new());
        let emit = |b: &[u8]| out.borrow_mut().extend_from_slice(b);
        h.data("s", b"one ", emit);
        h.hold("s", || out.borrow_mut().extend_from_slice(b"|"));
        h.data("s", b"two ", emit);
        h.data("other", b"x", |_| {}); // other sessions are untouched
        h.data("s", b"three", emit);
        assert_eq!(&*out.borrow(), b"one |");
        assert!(h.is_held("s"));
        h.release("s", emit, |_| panic!("not closed"));
        assert_eq!(&*out.borrow(), b"one |two three");
        h.data("s", b"!", emit);
        assert_eq!(&*out.borrow(), b"one |two three!");
        assert!(!h.is_held("s"));
    }

    #[test]
    fn a_close_while_held_comes_after_the_bytes() {
        let h = Holds::default();
        let log = RefCell::new(Vec::<String>::new());
        h.hold("s", || {});
        h.data("s", b"bye", |_| {});
        h.closed("s", "exit 0", |_| panic!("must wait"));
        h.release("s", |b| log.borrow_mut().push(String::from_utf8_lossy(b).into()), |r| log.borrow_mut().push(r.into()));
        assert_eq!(*log.borrow(), ["bye", "exit 0"]);
    }

    #[test]
    fn a_flood_while_held_keeps_the_tail() {
        let h = Holds::default();
        h.hold("s", || {});
        h.data("s", &vec![b'a'; HOLD_CAP], |_| {});
        h.data("s", b"END", |_| {});
        let mut got = Vec::new();
        h.release("s", |b| got = b.to_vec(), |_| {});
        assert_eq!(got.len(), HOLD_CAP);
        assert!(got.ends_with(b"END"));
    }

    #[test]
    fn stash_is_one_time() {
        let s = Stash::default();
        let t = s.put("{}".into());
        assert_eq!(s.take(&t).as_deref(), Some("{}"));
        assert_eq!(s.take(&t), None);
    }

    #[test]
    fn window_under_the_cursor_skips_the_source_and_prefers_main() {
        let wins = vec![
            ("tab-a".to_string(), Rect { x: 0, y: 0, w: 800, h: 600 }),
            ("main".to_string(), Rect { x: 100, y: 100, w: 800, h: 600 }),
            ("tab-b".to_string(), Rect { x: 2000, y: 0, w: 800, h: 600 }),
        ];
        assert_eq!(window_at(&wins, "tab-b", 150.0, 150.0), Some("main"));
        assert_eq!(window_at(&wins, "main", 150.0, 150.0), Some("tab-a"));
        assert_eq!(window_at(&wins, "tab-a", 50.0, 50.0), None);
        assert_eq!(window_at(&wins, "main", 2100.0, 10.0), Some("tab-b"));
        assert_eq!(window_at(&wins, "main", 5000.0, 10.0), None);
    }
}
