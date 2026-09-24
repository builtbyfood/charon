//! Recursive, multi-item transfers for the file pane. Works on the `Fs` trait so
//! it covers all backends (SFTP, sudo-sftp, ShellFs); ShellFs directories take a
//! `tar | base64` fast path (per-file base64 is too slow). Files transfer with
//! bounded concurrency, mode/mtime are preserved on request, unreadable entries
//! are skipped with a warning instead of aborting the tree, and cancellation
//! stops the whole tree.
//!
//! Progress is reported as `TreeEvent`s through a callback; the command layer
//! turns them into Tauri events and the tests collect them into a Vec.

use crate::sftp::{Fs, Transfer};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Conflict {
    Overwrite,
    Skip,
    Rename,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct TreeOpts {
    /// Max files transferring at once.
    pub parallel: usize,
    /// Follow symlinks (recurse into / copy the target); else skip them.
    pub follow_symlinks: bool,
    pub preserve_mode: bool,
    pub preserve_mtime: bool,
    pub conflict: Conflict,
}

impl Default for TreeOpts {
    fn default() -> Self {
        Self { parallel: 4, follow_symlinks: true, preserve_mode: false, preserve_mtime: true, conflict: Conflict::Overwrite }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlanFile {
    /// Path relative to the destination directory (includes the top-level name).
    pub rel: String,
    pub size: u64,
}

/// Reported to the caller as the transfer runs.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum TreeEvent {
    /// The walk is done: the file list and total bytes. `dest` is the resolved
    /// top-level name (may differ from the source after a Rename).
    Plan { files: Vec<PlanFile>, bytes_total: u64, dest: String },
    /// A file's state changed. `state`: "running" | "done" | "failed" | "skipped".
    File { rel: String, done: u64, total: u64, state: &'static str, error: Option<String> },
    /// A non-fatal problem (unreadable entry, skipped symlink…).
    Warn { message: String },
    /// The whole item finished.
    Done { files_done: u64, files_total: u64, bytes_done: u64, bytes_total: u64, failed: Vec<String>, cancelled: bool },
}

pub type Reporter = Arc<dyn Fn(TreeEvent) + Send + Sync>;

/// Cap for the ShellFs tar buffer (held in memory as base64); larger dirs fall
/// back to the per-file walk.
const TAR_LIMIT: u64 = 256 * 1024 * 1024;

struct FilePlan {
    rel: String,
    /// Source path (remote for download, local for upload).
    src: String,
    size: u64,
    mode: u32,
    mtime: u64,
}

/// Shared running counters for a tree.
struct Counts {
    files_done: AtomicU64,
    bytes_done: AtomicU64,
}

fn base_name(path: &str) -> String {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Split "name.ext" into ("name", ".ext"); a dotfile or extensionless name keeps
/// an empty extension.
fn split_ext(name: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    }
}

/// Pick a free "name" / "name (2)" / … under `exists(candidate)`.
fn unique_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    for i in 2.. {
        let candidate = format!("{stem} ({i}){ext}");
        if !exists(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

// ---------------------------------------------------------------- download

/// Download `remote_root` (file or dir) into the local directory `dest_dir`.
pub async fn download_tree(
    fs: Arc<dyn Fs>,
    remote_root: &str,
    dest_dir: &Path,
    opts: &TreeOpts,
    cancel: Arc<AtomicBool>,
    report: Reporter,
    only: Option<&HashSet<String>>,
) -> Result<()> {
    let root = fs.stat(remote_root).await.with_context(|| format!("stat {remote_root}"))?;
    let root_is_dir = root.kind == "dir" || (root.kind == "symlink" && root.target_kind == Some("dir"));

    // Resolve the top-level destination name against the conflict policy.
    let top = base_name(remote_root);
    let dest_top = match resolve_conflict(&top, opts.conflict, |n| dest_dir.join(n).exists()) {
        Some(name) => name,
        None => {
            report(TreeEvent::Warn { message: format!("{top}: exists, skipped") });
            report(TreeEvent::Done { files_done: 0, files_total: 0, bytes_done: 0, bytes_total: 0, failed: vec![], cancelled: false });
            return Ok(());
        }
    };

    // ShellFs fast path: a whole directory as one tar stream.
    if root_is_dir && only.is_none() {
        if let Some(tar) = fs.download_dir_tar(remote_root, TAR_LIMIT).await.unwrap_or(None) {
            // Decode + extract off the async reactor: it is synchronous file I/O.
            let (dest_dir, dest_top, opts, report) = (dest_dir.to_path_buf(), dest_top.clone(), *opts, report.clone());
            return tokio::task::spawn_blocking(move || extract_tar(tar, &dest_dir, &dest_top, &opts, &report)).await?;
        }
    }

    // Walk the remote tree into a flat plan.
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    walk_remote(fs.as_ref(), &root, remote_root, &dest_top, opts, &report, &mut files, &mut dirs).await;
    if let Some(only) = only {
        files.retain(|f| only.contains(&f.rel));
    }

    let bytes_total: u64 = files.iter().map(|f| f.size).sum();
    report(TreeEvent::Plan {
        files: files.iter().map(|f| PlanFile { rel: f.rel.clone(), size: f.size }).collect(),
        bytes_total,
        dest: dest_top.clone(),
    });

    // Create the local directory tree first.
    for d in &dirs {
        let _ = tokio::fs::create_dir_all(dest_dir.join(&d.rel)).await;
    }
    if root_is_dir {
        let _ = tokio::fs::create_dir_all(dest_dir.join(&dest_top)).await;
    }

    let counts = Arc::new(Counts { files_done: AtomicU64::new(0), bytes_done: AtomicU64::new(0) });
    let failed = run_pool(opts.parallel, cancel.clone(), files, |f| {
        let (fs, report, counts, cancel, dest_dir) = (fs.clone(), report.clone(), counts.clone(), cancel.clone(), dest_dir.to_path_buf());
        async move {
            let local = dest_dir.join(&f.rel);
            if let Some(parent) = local.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let t = file_transfer(&f.rel, report.clone(), cancel.clone());
            match fs.download(&f.src, &local, &t).await {
                Ok(()) => {
                    if opts.preserve_mode && f.mode != 0 {
                        let _ = set_local_mode(&local, f.mode).await;
                    }
                    if opts.preserve_mtime && f.mtime != 0 {
                        let _ = filetime_set(&local, f.mtime).await;
                    }
                    counts.files_done.fetch_add(1, Ordering::Relaxed);
                    counts.bytes_done.fetch_add(f.size, Ordering::Relaxed);
                    report(TreeEvent::File { rel: f.rel.clone(), done: f.size, total: f.size, state: "done", error: None });
                    None
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    report(TreeEvent::File { rel: f.rel.clone(), done: 0, total: f.size, state: "failed", error: Some(msg.clone()) });
                    report(TreeEvent::Warn { message: format!("{}: {msg}", f.rel) });
                    Some(f.rel.clone())
                }
            }
        }
    })
    .await;

    report(TreeEvent::Done {
        files_done: counts.files_done.load(Ordering::Relaxed),
        files_total: counts.files_done.load(Ordering::Relaxed) + failed.len() as u64,
        bytes_done: counts.bytes_done.load(Ordering::Relaxed),
        bytes_total,
        failed,
        cancelled: cancel.load(Ordering::Relaxed),
    });
    Ok(())
}

// ---------------------------------------------------------------- upload

/// Upload `local_root` (file or dir) into the remote directory `dest_remote_dir`.
pub async fn upload_tree(
    fs: Arc<dyn Fs>,
    local_root: &Path,
    dest_remote_dir: &str,
    opts: &TreeOpts,
    cancel: Arc<AtomicBool>,
    report: Reporter,
    only: Option<&HashSet<String>>,
) -> Result<()> {
    let meta = tokio::fs::symlink_metadata(local_root).await.with_context(|| format!("stat {}", local_root.display()))?;
    let root_is_dir = meta.is_dir();
    let top = base_name(&local_root.to_string_lossy());

    // Conflict against the remote destination (async stat, so resolved here
    // rather than through the sync `resolve_conflict`).
    let remote_exists = |n: &str| {
        let fs = fs.clone();
        let p = fs.join(dest_remote_dir, n);
        async move { fs.stat(&p).await.is_ok() }
    };
    let dest_top = if !remote_exists(&top).await {
        top.clone()
    } else {
        match opts.conflict {
            Conflict::Overwrite => top.clone(),
            Conflict::Skip => {
                report(TreeEvent::Warn { message: format!("{top}: exists on the remote, skipped") });
                report(TreeEvent::Done { files_done: 0, files_total: 0, bytes_done: 0, bytes_total: 0, failed: vec![], cancelled: false });
                return Ok(());
            }
            Conflict::Rename => {
                let mut chosen = top.clone();
                let (stem, ext) = split_ext(&top);
                for i in 2.. {
                    let cand = format!("{stem} ({i}){ext}");
                    if !remote_exists(&cand).await {
                        chosen = cand;
                        break;
                    }
                }
                chosen
            }
        }
    };

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    walk_local(local_root, &dest_top, opts, &report, &mut files, &mut dirs);
    if let Some(only) = only {
        files.retain(|f| only.contains(&f.rel));
    }
    let bytes_total: u64 = files.iter().map(|f| f.size).sum();
    report(TreeEvent::Plan {
        files: files.iter().map(|f| PlanFile { rel: f.rel.clone(), size: f.size }).collect(),
        bytes_total,
        dest: dest_top.clone(),
    });

    // Create remote dirs (parents first — walk_local yields them shallow-first).
    if root_is_dir {
        let _ = fs.mkdir(&fs.join(dest_remote_dir, &dest_top)).await;
    }
    for d in &dirs {
        let _ = fs.mkdir(&remote_join(fs.as_ref(), dest_remote_dir, &d.rel)).await;
    }

    let counts = Arc::new(Counts { files_done: AtomicU64::new(0), bytes_done: AtomicU64::new(0) });
    let failed = run_pool(opts.parallel, cancel.clone(), files, |f| {
        let (fs, report, counts, cancel, dest_remote_dir) =
            (fs.clone(), report.clone(), counts.clone(), cancel.clone(), dest_remote_dir.to_string());
        async move {
            let remote = remote_join(fs.as_ref(), &dest_remote_dir, &f.rel);
            let t = file_transfer(&f.rel, report.clone(), cancel.clone());
            match fs.upload(Path::new(&f.src), &remote, &t).await {
                Ok(()) => {
                    if opts.preserve_mode && f.mode != 0 {
                        let _ = fs.chmod(&remote, f.mode).await;
                    }
                    if opts.preserve_mtime && f.mtime != 0 {
                        let _ = fs.set_times(&remote, f.mtime).await;
                    }
                    counts.files_done.fetch_add(1, Ordering::Relaxed);
                    counts.bytes_done.fetch_add(f.size, Ordering::Relaxed);
                    report(TreeEvent::File { rel: f.rel.clone(), done: f.size, total: f.size, state: "done", error: None });
                    None
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    report(TreeEvent::File { rel: f.rel.clone(), done: 0, total: f.size, state: "failed", error: Some(msg.clone()) });
                    report(TreeEvent::Warn { message: format!("{}: {msg}", f.rel) });
                    Some(f.rel.clone())
                }
            }
        }
    })
    .await;

    let files_total = counts.files_done.load(Ordering::Relaxed) + failed.len() as u64;
    report(TreeEvent::Done {
        files_done: counts.files_done.load(Ordering::Relaxed),
        files_total,
        bytes_done: counts.bytes_done.load(Ordering::Relaxed),
        bytes_total,
        failed,
        cancelled: cancel.load(Ordering::Relaxed),
    });
    Ok(())
}

// ---------------------------------------------------------------- helpers

fn resolve_conflict(name: &str, policy: Conflict, exists: impl Fn(&str) -> bool) -> Option<String> {
    if !exists(name) {
        return Some(name.to_string());
    }
    match policy {
        Conflict::Overwrite => Some(name.to_string()),
        Conflict::Skip => None,
        Conflict::Rename => Some(unique_name(name, exists)),
    }
}

/// A per-file `Transfer` whose progress becomes `TreeEvent::File` "running" events.
fn file_transfer(rel: &str, report: Reporter, cancel: Arc<AtomicBool>) -> Transfer {
    let rel_owned = rel.to_string();
    Transfer::new(
        rel.to_string(),
        Some(Box::new(move |p| {
            if p.state == "running" {
                report(TreeEvent::File { rel: rel_owned.clone(), done: p.done, total: p.total, state: "running", error: None });
            }
        })),
        cancel,
    )
}

/// Run `job` over `items` with at most `parallel` at once, stopping early on
/// cancel; returns the rels that failed.
async fn run_pool<F, Fut>(parallel: usize, cancel: Arc<AtomicBool>, items: Vec<FilePlan>, job: F) -> Vec<String>
where
    F: Fn(FilePlan) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    use futures::stream::{self, StreamExt};
    let failed = Arc::new(std::sync::Mutex::new(Vec::new()));
    stream::iter(items)
        .for_each_concurrent(parallel.max(1), |f| {
            let (cancel, failed, fut) = (cancel.clone(), failed.clone(), job(f));
            async move {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(rel) = fut.await {
                    failed.lock().unwrap().push(rel);
                }
            }
        })
        .await;
    Arc::try_unwrap(failed).unwrap().into_inner().unwrap()
}

/// BFS the remote tree, appending files and dirs (shallow-first).
async fn walk_remote(
    fs: &dyn Fs,
    root: &crate::sftp::Entry,
    root_path: &str,
    dest_top: &str,
    opts: &TreeOpts,
    report: &Reporter,
    files: &mut Vec<FilePlan>,
    dirs: &mut Vec<FilePlan>,
) {
    let root_is_dir = root.kind == "dir" || (root.kind == "symlink" && root.target_kind == Some("dir"));
    if !root_is_dir {
        files.push(FilePlan { rel: dest_top.to_string(), src: root_path.to_string(), size: root.size, mode: root.mode, mtime: root.mtime });
        return;
    }
    // queue of (remote path, rel prefix)
    let mut queue = vec![(root_path.to_string(), dest_top.to_string())];
    while let Some((dir, rel)) = queue.pop() {
        let listing = match fs.list(&dir).await {
            Ok(l) => l,
            Err(e) => {
                report(TreeEvent::Warn { message: format!("{rel}: {e:#}") });
                continue;
            }
        };
        for e in listing.entries {
            let child_rel = format!("{rel}/{}", e.name);
            let is_dir = e.kind == "dir" || (e.kind == "symlink" && e.target_kind == Some("dir"));
            if e.kind == "symlink" && !opts.follow_symlinks {
                report(TreeEvent::Warn { message: format!("{child_rel}: symlink skipped") });
                continue;
            }
            if e.kind == "other" {
                report(TreeEvent::Warn { message: format!("{child_rel}: special file skipped") });
                continue;
            }
            if is_dir {
                dirs.push(FilePlan { rel: child_rel.clone(), src: e.path.clone(), size: 0, mode: e.mode, mtime: e.mtime });
                queue.push((e.path, child_rel));
            } else {
                files.push(FilePlan { rel: child_rel, src: e.path, size: e.size, mode: e.mode, mtime: e.mtime });
            }
        }
    }
}

/// Walk the local tree with std::fs; dirs shallow-first (BFS), matching walk_remote.
fn walk_local(root: &Path, dest_top: &str, opts: &TreeOpts, report: &Reporter, files: &mut Vec<FilePlan>, dirs: &mut Vec<FilePlan>) {
    let root_meta = match std::fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(e) => {
            report(TreeEvent::Warn { message: format!("{}: {e}", root.display()) });
            return;
        }
    };
    if root_meta.is_file() {
        let (size, mtime, mode) = local_meta(root);
        files.push(FilePlan { rel: dest_top.to_string(), src: root.to_string_lossy().into_owned(), size, mode, mtime });
        return;
    }
    // queue of (local dir, rel prefix)
    let mut queue = vec![(root.to_path_buf(), dest_top.to_string())];
    while let Some((dir, rel)) = queue.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                report(TreeEvent::Warn { message: format!("{rel}: {e}") });
                continue;
            }
        };
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    report(TreeEvent::Warn { message: format!("{rel}: {e}") });
                    continue;
                }
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = format!("{rel}/{name}");
            let link_meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    report(TreeEvent::Warn { message: format!("{child_rel}: {e}") });
                    continue;
                }
            };
            if link_meta.file_type().is_symlink() && !opts.follow_symlinks {
                report(TreeEvent::Warn { message: format!("{child_rel}: symlink skipped") });
                continue;
            }
            // Resolve through symlinks to classify (follow mode).
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    report(TreeEvent::Warn { message: format!("{child_rel}: {e}") });
                    continue;
                }
            };
            let (size, mtime, mode) = local_meta(&path);
            if meta.is_dir() {
                dirs.push(FilePlan { rel: child_rel.clone(), src: path.to_string_lossy().into_owned(), size: 0, mode, mtime });
                queue.push((path, child_rel));
            } else if meta.is_file() {
                files.push(FilePlan { rel: child_rel, src: path.to_string_lossy().into_owned(), size, mode, mtime });
            }
        }
    }
}

fn local_meta(path: &Path) -> (u64, u64, u32) {
    let m = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return (0, 0, 0),
    };
    let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o7777
    };
    #[cfg(not(unix))]
    let mode = 0u32;
    (m.len(), mtime, mode)
}

fn remote_join(fs: &dyn Fs, base: &str, rel: &str) -> String {
    // rel is "/"-separated; fold it onto the remote base with the fs's joiner.
    let mut path = base.to_string();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        path = fs.join(&path, seg);
    }
    path
}

async fn set_local_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777)).await?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Synchronous chmod for the (blocking) tar extractor; no-op off Unix.
fn set_local_mode_sync(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

async fn filetime_set(path: &Path, mtime: u64) -> Result<()> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(mtime as i64, 0))).await??;
    Ok(())
}

/// Extract a `tar` stream (ShellFs fast path) into `dest_dir/dest_top`.
fn extract_tar(tar_bytes: Vec<u8>, dest_dir: &Path, dest_top: &str, opts: &TreeOpts, report: &Reporter) -> Result<()> {
    use std::io::Read;
    // The tar was `tar -cf - <name>`, so entries are prefixed with the source name;
    // strip that first component and re-root under dest_top.
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    let mut files_done = 0u64;
    let mut bytes_done = 0u64;
    let mut plan = Vec::new();
    let mut failed = Vec::new();
    for entry in archive.entries().context("read tar")? {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                report(TreeEvent::Warn { message: format!("tar entry: {e}") });
                continue;
            }
        };
        let path = entry.path().context("tar path")?.into_owned();
        let mut comps = path.components();
        comps.next(); // drop the source top-level name
        let sub: PathBuf = comps.as_path().to_path_buf();
        if sub.as_os_str().is_empty() {
            continue;
        }
        let out = dest_dir.join(dest_top).join(&sub);
        let is_dir = entry.header().entry_type().is_dir();
        if is_dir {
            let _ = std::fs::create_dir_all(&out);
            continue;
        }
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut buf = Vec::new();
        if let Err(e) = entry.read_to_end(&mut buf) {
            failed.push(sub.to_string_lossy().into_owned());
            report(TreeEvent::Warn { message: format!("{}: {e}", sub.display()) });
            continue;
        }
        if let Err(e) = std::fs::write(&out, &buf) {
            failed.push(sub.to_string_lossy().into_owned());
            report(TreeEvent::Warn { message: format!("{}: {e}", out.display()) });
            continue;
        }
        let rel = format!("{dest_top}/{}", sub.to_string_lossy().replace('\\', "/"));
        if opts.preserve_mode {
            if let Ok(mode) = entry.header().mode() {
                set_local_mode_sync(&out, mode);
            }
        }
        if opts.preserve_mtime {
            if let Ok(mtime) = entry.header().mtime() {
                let _ = filetime::set_file_mtime(&out, filetime::FileTime::from_unix_time(mtime as i64, 0));
            }
        }
        files_done += 1;
        bytes_done += buf.len() as u64;
        plan.push(PlanFile { rel: rel.clone(), size: buf.len() as u64 });
        report(TreeEvent::File { rel, done: buf.len() as u64, total: buf.len() as u64, state: "done", error: None });
    }
    report(TreeEvent::Plan { files: plan, bytes_total: bytes_done, dest: dest_top.to_string() });
    report(TreeEvent::Done {
        files_done,
        files_total: files_done + failed.len() as u64,
        bytes_done,
        bytes_total: bytes_done,
        failed,
        cancelled: false,
    });
    Ok(())
}

#[cfg(test)]
mod tests;
