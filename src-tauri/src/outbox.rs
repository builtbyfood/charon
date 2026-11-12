//! Where each session's output goes: the channel its Terminal attached
//! (`session_attach`), or, until one is attached, a buffer. A session starts
//! producing output (a login banner, the first prompt) as soon as it opens, which
//! can be before its tab has attached; those bytes wait here and go out first, in
//! order, when the channel arrives, so nothing is lost or reordered. Pure: the
//! sink is any `Fn(Msg)`, so tests need no tauri types.

use std::collections::HashMap;
use std::sync::Mutex;

/// Cap on output kept for a session nobody has attached to (a flood before the
/// tab is up, or a session whose tab never appears).
pub const PENDING_CAP: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Data { seq: u64, t: f64, bytes: Vec<u8> },
    Closed { reason: String },
}

pub type Sink = Box<dyn Fn(Msg) + Send + Sync>;

enum Slot {
    Attached(Sink),
    Pending {
        msgs: Vec<Msg>,
        bytes: usize,
        dropped: usize,
    },
}

/// How a message was routed (for the output trace).
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Sent,
    Buffered,
    /// Buffer full: the oldest output made room.
    BufferedDropping,
}

#[derive(Default)]
pub struct Outbox(Mutex<HashMap<String, Slot>>);

impl Outbox {
    /// Send to the attached sink, or keep it until one attaches. Under the lock,
    /// so an `attach` racing with this sees the message either buffered (and
    /// flushed) or after it, never both or neither.
    pub fn send(&self, id: &str, msg: Msg) -> Route {
        let mut m = self.0.lock().unwrap();
        let slot = m.entry(id.to_string()).or_insert_with(|| Slot::Pending {
            msgs: vec![],
            bytes: 0,
            dropped: 0,
        });
        let closing = matches!(msg, Msg::Closed { .. });
        let route = match slot {
            Slot::Attached(sink) => {
                sink(msg);
                Route::Sent
            }
            Slot::Pending {
                msgs,
                bytes,
                dropped,
            } => {
                if let Msg::Data { bytes: b, .. } = &msg {
                    *bytes += b.len();
                }
                msgs.push(msg);
                let mut route = Route::Buffered;
                while *bytes > PENDING_CAP && msgs.len() > 1 {
                    if let Msg::Data { bytes: b, .. } = msgs.remove(0) {
                        *bytes -= b.len();
                        *dropped += b.len();
                    }
                    route = Route::BufferedDropping;
                }
                return route; // a pending close stays until someone attaches
            }
        };
        if closing {
            m.remove(id); // closed and delivered: nothing more for this session
        }
        route
    }

    /// Route `id` to `sink` from now on (replacing a previous window's), after
    /// handing it everything buffered so far. Returns how many messages were
    /// flushed and how many bytes had been dropped for the cap.
    pub fn attach(&self, id: &str, sink: Sink) -> (usize, usize) {
        let mut m = self.0.lock().unwrap();
        let (flushed, dropped, closed) = match m.remove(id) {
            Some(Slot::Pending { msgs, dropped, .. }) => {
                let n = msgs.len();
                let closed = msgs.iter().any(|x| matches!(x, Msg::Closed { .. }));
                for x in msgs {
                    sink(x);
                }
                (n, dropped, closed)
            }
            _ => (0, 0, false),
        };
        if !closed {
            m.insert(id.to_string(), Slot::Attached(sink));
        }
        (flushed, dropped)
    }

    /// Forget `id` (its session is gone and will never attach).
    pub fn forget(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn data(seq: u64, s: &str) -> Msg {
        Msg::Data {
            seq,
            t: 0.0,
            bytes: s.as_bytes().to_vec(),
        }
    }
    fn recorder() -> (Arc<Mutex<Vec<Msg>>>, Sink) {
        let got = Arc::new(Mutex::new(vec![]));
        let g = got.clone();
        (got, Box::new(move |m| g.lock().unwrap().push(m)))
    }

    #[test]
    fn output_before_attach_is_delivered_first_and_in_order() {
        let o = Outbox::default();
        assert_eq!(o.send("s", data(0, "banner")), Route::Buffered);
        assert_eq!(o.send("s", data(1, "$ ")), Route::Buffered);
        let (got, sink) = recorder();
        assert_eq!(o.attach("s", sink), (2, 0));
        assert_eq!(o.send("s", data(2, "ls")), Route::Sent);
        assert_eq!(
            *got.lock().unwrap(),
            vec![data(0, "banner"), data(1, "$ "), data(2, "ls")]
        );
    }

    #[test]
    fn a_close_before_attach_is_delivered_and_ends_the_session() {
        let o = Outbox::default();
        o.send("s", data(0, "bye"));
        o.send(
            "s",
            Msg::Closed {
                reason: "exit 0".into(),
            },
        );
        let (got, sink) = recorder();
        o.attach("s", sink);
        assert_eq!(got.lock().unwrap().len(), 2);
        // Nothing left behind for the closed session.
        assert!(o.0.lock().unwrap().is_empty());
    }

    #[test]
    fn reattach_moves_the_stream_and_close_cleans_up() {
        let o = Outbox::default();
        let (a, sa) = recorder();
        let (b, sb) = recorder();
        o.attach("s", sa);
        o.send("s", data(0, "x"));
        o.attach("s", sb);
        o.send("s", data(1, "y"));
        o.send(
            "s",
            Msg::Closed {
                reason: "gone".into(),
            },
        );
        assert_eq!(a.lock().unwrap().len(), 1);
        assert_eq!(b.lock().unwrap().len(), 2);
        assert!(o.0.lock().unwrap().is_empty());
    }

    #[test]
    fn unattached_output_is_capped_oldest_first() {
        let o = Outbox::default();
        let chunk = "x".repeat(PENDING_CAP / 2);
        o.send("s", data(0, &chunk));
        o.send("s", data(1, &chunk));
        assert_eq!(o.send("s", data(2, "tail")), Route::BufferedDropping);
        let (got, sink) = recorder();
        assert_eq!(o.attach("s", sink), (2, PENDING_CAP / 2));
        let seqs: Vec<u64> = got
            .lock()
            .unwrap()
            .iter()
            .map(|m| match m {
                Msg::Data { seq, .. } => *seq,
                _ => 99,
            })
            .collect();
        assert_eq!(seqs, vec![1, 2]);
    }
}
