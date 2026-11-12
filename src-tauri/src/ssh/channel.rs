//! The task that owns the interactive shell channel: output to the UI (coalesced),
//! input from `Cmd::Write`, hidden commands typed and captured (`hidden::Capture`).

use crate::session::SessionManager;
use anyhow::{anyhow, Result};
use russh::client::Msg;
use russh::{Channel, ChannelMsg};
use tokio::sync::{mpsc, oneshot};

/// Commands sent to the task that owns the channel.
pub(super) enum Cmd {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
    /// Type `line`, capture between sentinels, hide the whole exchange from the UI.
    Capture {
        line: Vec<u8>,
        nonce: String,
        reply: oneshot::Sender<Result<(Vec<u8>, i32)>>,
    },
}

/// The hidden command being captured, and where its result goes.
type CaptureSlot = Option<(
    crate::hidden::Capture,
    oneshot::Sender<Result<(Vec<u8>, i32)>>,
)>;

/// Run the channel until it closes or `Cmd::Close`.
pub(super) fn spawn(
    app: tauri::AppHandle,
    id: String,
    mut channel: Channel<Msg>,
    mut rx: mpsc::Receiver<Cmd>,
) {
    tokio::spawn(async move {
        // Coalesce output into ≤ 1 UI event per 16 ms (see session::Coalescer).
        let mut out = crate::session::Coalescer::new(id.clone(), {
            let (app, id) = (app.clone(), id.clone());
            move |b: &[u8]| SessionManager::emit_data(&app, &id, b)
        });
        let mut capture: CaptureSlot = None;
        // User keystrokes / pastes that arrive while a hidden command is capturing
        // are held here and flushed once it finishes, so they never interleave with
        // (or get dropped by) the capture window.
        let mut pending_writes: Vec<u8> = Vec::new();
        // Hidden commands are typed into the shell: only at an empty prompt.
        let mut prompt = crate::hidden::PromptState::default();
        let finish = |c: CaptureSlot, r: Result<(), String>| {
            if let Some((cap, reply)) = c {
                let _ = reply.send(r.map(|_| cap.into_result()).map_err(|e| anyhow!(e)));
            }
        };
        loop {
            let flush_due = out.due_in();
            tokio::select! {
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        prompt.output(&data);
                        match capture.as_mut() {
                            Some((cap, _)) => {
                                let show = cap.feed(&data);
                                // Keep ordering: drain any coalesced normal output first.
                                if !show.is_empty() { out.flush_close(); SessionManager::emit_data(&app, &id, &show); }
                                // Answer swallowed cursor queries (pwsh's PSReadLine on a Unix host).
                                if let Some((r, n)) = cap.take_replies() { prompt.answered(n); let _ = channel.data(&r[..]).await; }
                                if cap.done() { finish(capture.take(), Ok(())); }
                            }
                            None => out.push(&data),
                        }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        out.flush_close();
                        SessionManager::emit_closed(&app, &id, &format!("exit {exit_status}"));
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        out.flush_close();
                        SessionManager::emit_closed(&app, &id, "closed");
                        break;
                    }
                    _ => {}
                },
                cmd = rx.recv() => match cmd {
                    // Queue input while a hidden command captures; flush after (below).
                    Some(Cmd::Write(b)) => {
                        prompt.input(&b);
                        if capture.is_some() { pending_writes.extend_from_slice(&b); }
                        else { let _ = channel.data(&b[..]).await; }
                    }
                    Some(Cmd::Resize(c, r)) => { let _ = channel.window_change(c as u32, r as u32, 0, 0).await; }
                    Some(Cmd::Capture { line, nonce, reply }) => {
                        out.flush_close(); // don't let buffered output land inside the capture window
                        if capture.is_some() {
                            let _ = reply.send(Err(anyhow!("another hidden command is still running")));
                        } else if let Err(why) = prompt.ready() {
                            tracing::info!(session = %id, "hidden command refused: {why}");
                            let _ = reply.send(Err(anyhow!("{why}")));
                        } else if channel.data(&line[..]).await.is_err() {
                            let _ = reply.send(Err(anyhow!("channel gone")));
                        } else {
                            capture = Some((crate::hidden::Capture::new(&nonce), reply));
                        }
                    }
                    Some(Cmd::Close) | None => {
                        finish(capture.take(), Err("session closed".into()));
                        let _ = channel.eof().await;
                        let _ = channel.close().await;
                        break;
                    }
                },
                // Quiet-based tail end and the overall timeout.
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)), if capture.is_some() => {
                    if let Some(r) = capture.as_mut().and_then(|(c, _)| c.tick()) {
                        finish(capture.take(), r);
                    }
                }
                // Flush coalesced output once its window elapses.
                _ = tokio::time::sleep(flush_due.unwrap_or(std::time::Duration::from_millis(16))), if flush_due.is_some() => {
                    out.flush_timer();
                }
            }
            // A hidden command just finished: flush any input queued during it.
            if capture.is_none() && !pending_writes.is_empty() {
                let out = std::mem::take(&mut pending_writes);
                let _ = channel.data(&out[..]).await;
            }
        }
    });
}
