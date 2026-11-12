//! Linux: force GTK to repaint the webview after output.
//!
//! On some Linux setups (seen on Fedora 44, Intel i915, WebKitGTK 2.54) the page
//! updates (the output trace shows every chunk received, parsed and painted in
//! under 50 ms) but WebKitGTK doesn't present the new frame to GTK until an
//! input event arrives. As a workaround, every output chunk `poke`s a repaint
//! thread, which asks GTK to redraw the webview at most once per 16 ms (the poke
//! during the wait queues one more, so the last chunk is always covered).
//!
//! `Mode` escalates: `draw` = `gtk_widget_queue_draw` on the webview, `resize` =
//! also `queue_resize`, `nudge` = also grow the window by 1 px and back (to the
//! size remembered before the first nudge of a burst: Wayland applies resizes
//! asynchronously, so reading the size back mid-burst returns the grown one). Set by
//! the profile's `terminal.linux_repaint` (Settings → Terminal), overridden by
//! `CHARON_REPAINT=off|draw|resize|nudge`. Does nothing on other OSes.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Off = 0,
    Draw = 1,
    Resize = 2,
    Nudge = 3,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" => Some(Mode::Off),
            "draw" | "on" | "1" | "true" => Some(Mode::Draw),
            "resize" => Some(Mode::Resize),
            "nudge" => Some(Mode::Nudge),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Draw => "draw",
            Mode::Resize => "resize",
            Mode::Nudge => "nudge",
        }
    }
    fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::Draw,
            2 => Mode::Resize,
            3 => Mode::Nudge,
            _ => Mode::Off,
        }
    }
}

/// The mode in effect: the environment wins over the profile; an unknown profile
/// value means the default, off (the stall it was for turned out to be the DMABUF
/// renderer being forced off, `webkitenv.rs`; this stays as a fallback).
pub fn effective(profile: &str, env: Option<&str>) -> Mode {
    if let Some(m) = env.and_then(Mode::parse) {
        return m;
    }
    Mode::parse(profile).unwrap_or(Mode::Off)
}

static MODE: AtomicU8 = AtomicU8::new(0);

/// Apply the profile's value (at start and whenever the profile is saved).
pub fn configure(profile: &str) -> Mode {
    let env = std::env::var("CHARON_REPAINT").ok();
    let m = if cfg!(target_os = "linux") {
        effective(profile, env.as_deref())
    } else {
        Mode::Off
    };
    let old = Mode::from_u8(MODE.swap(m as u8, Ordering::Relaxed));
    if old != m {
        tracing::info!(
            "[charon] repaint: mode={}{}",
            m.name(),
            if env.is_some() {
                " (CHARON_REPAINT)"
            } else {
                ""
            }
        );
    }
    m
}

pub fn mode() -> Mode {
    Mode::from_u8(MODE.load(Ordering::Relaxed))
}

#[cfg(target_os = "linux")]
static TX: std::sync::OnceLock<std::sync::mpsc::SyncSender<&'static str>> =
    std::sync::OnceLock::new();

/// Something changed on screen (`why`: "output", "ticker"): repaint soon.
pub fn poke(why: &'static str) {
    if mode() == Mode::Off {
        return;
    }
    #[cfg(target_os = "linux")]
    if let Some(tx) = TX.get() {
        let _ = tx.try_send(why); // full = a repaint is already queued
    }
    #[cfg(not(target_os = "linux"))]
    let _ = why;
}

/// Start the repaint thread (Linux; once).
#[cfg(target_os = "linux")]
pub fn start(app: tauri::AppHandle) {
    use std::time::{Duration, Instant};
    let (tx, rx) = std::sync::mpsc::sync_channel::<&'static str>(1);
    if TX.set(tx).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("repaint".into())
        .spawn(move || {
            let (mut fired, mut since) = (0u64, Instant::now());
            let mut sizes = Sizes::new();
            while let Ok(why) = rx.recv() {
                let m = mode();
                if m == Mode::Off {
                    continue;
                }
                repaint_all(&app, m, &mut sizes);
                fired += 1;
                if crate::session::TRACE.load(Ordering::Relaxed) {
                    tracing::info!(
                        "[charon] repaint: fired mode={} why={why} n={fired}",
                        m.name()
                    );
                } else if since.elapsed() >= Duration::from_secs(5) {
                    tracing::info!(
                        "[charon] repaint: mode={} fired {fired} times in {:.0} s",
                        m.name(),
                        since.elapsed().as_secs_f64()
                    );
                    (fired, since) = (0, Instant::now());
                }
                std::thread::sleep(Duration::from_millis(16));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("repaint thread: {e}");
    }
}

/// Per window: its size before the current nudge burst, and the last nudge.
#[cfg(target_os = "linux")]
type Sizes = std::collections::HashMap<String, (tauri::PhysicalSize<u32>, std::time::Instant)>;

#[cfg(target_os = "linux")]
fn repaint_all(app: &tauri::AppHandle, m: Mode, sizes: &mut Sizes) {
    use tauri::Manager;
    for w in app.webview_windows().into_values() {
        // with_webview runs the closure on the GTK main thread.
        let r = w.with_webview(move |wv| {
            use gtk::prelude::WidgetExt;
            let view = wv.inner();
            view.queue_draw();
            if m >= Mode::Resize {
                view.queue_resize();
            }
        });
        if let Err(e) = r {
            tracing::warn!("[charon] repaint: {}: {e}", w.label());
            continue;
        }
        if m == Mode::Nudge {
            // A burst reuses the size from its start; after a quiet second the
            // window may have been resized by the user, so read it afresh.
            let now = std::time::Instant::now();
            let size = match sizes.get(w.label()) {
                Some((s, last))
                    if now.duration_since(*last) < std::time::Duration::from_secs(1) =>
                {
                    Some(*s)
                }
                _ => w.inner_size().ok(),
            };
            if let Some(size) = size {
                sizes.insert(w.label().to_string(), (size, now));
                let _ = w.set_size(tauri::PhysicalSize::new(size.width + 1, size.height));
                std::thread::sleep(std::time::Duration::from_millis(4));
                let _ = w.set_size(size);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_profile_and_default_is_off() {
        assert_eq!(effective("off", None), Mode::Off);
        assert_eq!(effective("resize", None), Mode::Resize);
        assert_eq!(effective("", None), Mode::Off);
        assert_eq!(effective("bogus", None), Mode::Off);
        assert_eq!(effective("off", Some("nudge")), Mode::Nudge);
        assert_eq!(effective("draw", Some("0")), Mode::Off);
        // An unparseable env value doesn't override.
        assert_eq!(effective("resize", Some("yes please")), Mode::Resize);
        assert!(Mode::Nudge > Mode::Resize && Mode::Resize > Mode::Draw);
        for m in [Mode::Off, Mode::Draw, Mode::Resize, Mode::Nudge] {
            assert_eq!(Mode::parse(m.name()), Some(m));
        }
    }
}
