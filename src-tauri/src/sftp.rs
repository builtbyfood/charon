//! File access behind the SFTP pane and the editor. One `Fs` per session kind:
//! `SftpFs` rides a subsystem channel on the session's existing SSH connection,
//! `LocalFs` is std::fs for local PTY tabs and local files. Transfers stream in
//! 256 KiB chunks and report progress as `sftp:progress` events; a cancel flag
//! per transfer is checked between chunks.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const CHUNK: usize = 256 * 1024;
/// Per-request SFTP timeout. russh-sftp's default is 10 s, which a big file's
/// close (or a read on a congested link) can exceed; dead links are caught by
/// keepalive instead.
pub const SFTP_REQUEST_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Full path, ready to pass back to any command.
    pub path: String,
    /// "file" | "dir" | "symlink" | "other". Symlinks report the target kind in `target_kind`.
    pub kind: &'static str,
    pub target_kind: Option<&'static str>,
    pub size: u64,
    /// Seconds since the epoch, 0 if unknown.
    pub mtime: u64,
    /// Permission bits (low 12 bits of st_mode); 0 when the backend has none (Windows).
    pub mode: u32,
    pub user: Option<String>,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    /// Canonical directory path.
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

#[async_trait]
pub trait Fs: Send + Sync {
    /// Starting directory for the pane (remote home / local home).
    async fn home(&self) -> Result<String>;
    async fn list(&self, path: &str) -> Result<Listing>;
    async fn stat(&self, path: &str) -> Result<Entry>;
    async fn mkdir(&self, path: &str) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    /// Files and empty directories only; the UI asks before calling.
    async fn delete(&self, path: &str, is_dir: bool) -> Result<()>;
    async fn chmod(&self, path: &str, mode: u32) -> Result<()>;
    /// Whole file into memory (editor). Refused above `limit` bytes.
    async fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>>;
    /// Atomic replace: write `path.charon-tmp`, then rename over `path`.
    async fn write(&self, path: &str, data: &[u8]) -> Result<()>;
    /// Stream a file out to a local path.
    async fn download(&self, path: &str, local: &Path, t: &Transfer) -> Result<()>;
    /// Stream a local file in.
    async fn upload(&self, local: &Path, path: &str, t: &Transfer) -> Result<()>;
    /// Join a directory and a child name the way this filesystem spells paths.
    fn join(&self, dir: &str, name: &str) -> String;
    /// Set a file/dir mtime (seconds since epoch); default no-op where unsupported.
    async fn set_times(&self, _path: &str, _mtime: u64) -> Result<()> {
        Ok(())
    }
    /// Whole-directory download as a tar stream, for backends where per-file is too
    /// slow (ShellFs). `None` = fall back to the generic recursive walk.
    async fn download_dir_tar(&self, _remote: &str, _limit: u64) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Copy one file to another path on this same filesystem (file pane copy/paste).
    /// Default: out to a local temp file and back in; backends override with a
    /// direct stream or a server-side `cp`.
    async fn copy_file(&self, from: &str, to: &str, t: &Transfer) -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("charon-copy-{}", uuid::Uuid::new_v4()));
        let r = async {
            self.download(from, &tmp, t).await?;
            self.upload(&tmp, to, t).await
        }
        .await;
        let _ = tokio::fs::remove_file(&tmp).await;
        r
    }
    /// Delete a file, symlink or whole directory tree. Symlinks are removed, never
    /// followed. Default: walk with `list` and delete children before parents.
    async fn remove_tree(&self, path: &str) -> Result<()> {
        let root = self.stat(path).await?;
        if root.kind != "dir" {
            return self.delete(path, false).await;
        }
        // Pre-order walk; reversed, every directory comes after everything inside it.
        let mut stack = vec![path.to_string()];
        let mut dirs = Vec::new();
        while let Some(d) = stack.pop() {
            for e in self.list(&d).await?.entries {
                if e.kind == "dir" {
                    stack.push(e.path);
                } else {
                    self.delete(&e.path, false).await.with_context(|| format!("delete {}", e.path))?;
                }
            }
            dirs.push(d);
        }
        for d in dirs.iter().rev() {
            self.delete(d, true).await.with_context(|| format!("delete {d}"))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- transfers

/// Progress reporting + cancellation for one transfer. Reports go through a
/// callback (the command layer turns them into Tauri events) so this module,
/// and the test binary, stay free of tauri types.
pub struct Transfer {
    pub id: String,
    on_progress: Option<Box<dyn Fn(Progress) + Send + Sync>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub id: String,
    pub done: u64,
    pub total: u64,
    /// "running" | "done" | "error" | "cancelled"
    pub state: &'static str,
    pub error: Option<String>,
}

impl Transfer {
    pub fn new(id: String, on_progress: Option<Box<dyn Fn(Progress) + Send + Sync>>, cancel: Arc<AtomicBool>) -> Self {
        Self { id, on_progress, cancel }
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn report(&self, done: u64, total: u64, state: &'static str, error: Option<String>) {
        if let Some(f) = &self.on_progress {
            f(Progress { id: self.id.clone(), done, total, state, error });
        }
    }
    /// Stop early with a clean status if the user cancelled.
    fn check(&self) -> Result<()> {
        if self.cancelled() {
            Err(anyhow!("cancelled"))
        } else {
            Ok(())
        }
    }
}

/// Live transfers, so the UI can cancel by id.
#[derive(Default)]
pub struct Transfers {
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Transfers {
    pub fn start(&self, id: &str) -> Arc<AtomicBool> {
        let f = Arc::new(AtomicBool::new(false));
        self.flags.lock().unwrap().insert(id.to_string(), f.clone());
        f
    }
    pub fn cancel(&self, id: &str) -> bool {
        match self.flags.lock().unwrap().get(id) {
            Some(f) => {
                f.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }
    pub fn finish(&self, id: &str) {
        self.flags.lock().unwrap().remove(id);
    }
}

/// Pump `src` into `dst`, reporting every chunk. Returns bytes copied. Does not
/// flush: a local file wants `flush`, an SFTP file wants `shutdown` (drain the
/// writes and close) — flushing one would also fsync it on the server.
async fn pump<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    src: &mut R,
    dst: &mut W,
    total: u64,
    t: &Transfer,
) -> Result<u64> {
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    t.report(0, total, "running", None);
    loop {
        t.check()?;
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await?;
        done += n as u64;
        t.report(done, total, "running", None);
    }
    Ok(done)
}

// ---------------------------------------------------------------- SFTP

pub struct SftpFs {
    pub sftp: Arc<SftpSession>,
}

impl SftpFs {
    pub fn new(sftp: SftpSession) -> Self {
        sftp.set_timeout(SFTP_REQUEST_TIMEOUT_SECS);
        SftpFs { sftp: Arc::new(sftp) }
    }
}

/// Write the sudo password (consumed by `sudo -S -p ''`) then start an SFTP
/// session on the same stream (sftp-server, now running as root, reads the rest).
/// Split out so it can be tested against a mock sftp-server over a duplex pipe.
pub async fn sudo_sftp_over<S>(mut stream: S, password: &str) -> Result<SftpFs>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    stream.write_all(format!("{password}\n").as_bytes()).await?;
    stream.flush().await?;
    let sftp = SftpSession::new(stream)
        .await
        .context("sudo sftp-server did not start (wrong password, or sftp-server missing)")?;
    Ok(SftpFs::new(sftp))
}

/// Just the permission bits of `a`, for a setstat that touches nothing else.
fn mode_of(a: &FileAttributes) -> FileAttributes {
    let mut m = FileAttributes::empty();
    m.permissions = a.permissions.map(|p| p & 0o7777);
    m
}

fn kind_of(a: &FileAttributes) -> &'static str {
    if a.is_dir() {
        "dir"
    } else if a.is_symlink() {
        "symlink"
    } else if a.is_regular() {
        "file"
    } else {
        "other"
    }
}

fn entry_from(name: String, path: String, a: &FileAttributes) -> Entry {
    Entry {
        name,
        path,
        kind: kind_of(a),
        target_kind: None,
        size: a.size.unwrap_or(0),
        mtime: a.mtime.unwrap_or(0) as u64,
        mode: a.permissions.unwrap_or(0) & 0o7777,
        user: a.user.clone(),
        group: a.group.clone(),
    }
}

/// ShellFs atomic replace: stat the original (owner, mode) first, decode into a temp
/// file given the original mode, `mv` it over, then chown + chmod the result (chown
/// first: it clears setuid/setgid). A new file keeps the shell's defaults.
/// Heredoc body follows the command line; the `; printf` end sentinel added by
/// hidden::wrap runs after the whole line, i.e. after the heredoc is consumed.
pub fn shell_write_cmd(path: &str, b64_body: &str) -> String {
    format!(
        "__o=$(stat -c %u:%g -- {p} 2>/dev/null); __m=$(stat -c %a -- {p} 2>/dev/null); \
         base64 -d <<'CHARON_B64' > {t} && {{ [ -z \"$__m\" ] || chmod \"$__m\" -- {t}; }} && mv -f -- {t} {p} \
         && {{ [ -z \"$__o\" ] || {{ chown \"$__o\" -- {p} && chmod \"$__m\" -- {p}; }}; }}; \
         __rc=$?; unset __o __m; (exit $__rc)\n{body}\nCHARON_B64",
        t = sq(&format!("{path}.charon-tmp")),
        p = sq(path),
        body = b64_body,
    )
}

pub fn posix_join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

pub fn posix_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None; // root
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".into()),
        Some(i) => Some(trimmed[..i].to_string()),
        None => None,
    }
}

#[async_trait]
impl Fs for SftpFs {
    async fn home(&self) -> Result<String> {
        Ok(self.sftp.canonicalize(".").await?)
    }

    async fn list(&self, path: &str) -> Result<Listing> {
        let canon = self.sftp.canonicalize(path).await.unwrap_or_else(|_| path.to_string());
        let mut entries = Vec::new();
        for e in self.sftp.read_dir(&canon).await? {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let full = posix_join(&canon, &name);
            let mut entry = entry_from(name, full.clone(), &e.metadata());
            if entry.kind == "symlink" {
                // What it points at decides whether double-click navigates or opens.
                entry.target_kind = Some(self.sftp.metadata(&full).await.map(|m| kind_of(&m)).unwrap_or("other"));
            }
            entries.push(entry);
        }
        Ok(Listing { parent: posix_parent(&canon), path: canon, entries })
    }

    async fn stat(&self, path: &str) -> Result<Entry> {
        let a = self.sftp.symlink_metadata(path).await?;
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let mut e = entry_from(name, path.to_string(), &a);
        if e.kind == "symlink" {
            e.target_kind = Some(self.sftp.metadata(path).await.map(|m| kind_of(&m)).unwrap_or("other"));
        }
        Ok(e)
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        Ok(self.sftp.create_dir(path).await?)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        Ok(self.sftp.rename(from, to).await?)
    }

    async fn delete(&self, path: &str, is_dir: bool) -> Result<()> {
        if is_dir {
            Ok(self.sftp.remove_dir(path).await?)
        } else {
            Ok(self.sftp.remove_file(path).await?)
        }
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let mut a = FileAttributes::empty();
        a.permissions = Some(mode & 0o7777);
        Ok(self.sftp.set_metadata(path, a).await?)
    }

    async fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let size = self.sftp.metadata(path).await?.size.unwrap_or(0);
        if size > limit {
            return Err(anyhow!("{path} is {size} bytes; the editor limit is {limit}"));
        }
        let mut f = self.sftp.open(path).await?;
        let mut out = Vec::with_capacity(size as usize);
        f.read_to_end(&mut out).await?;
        Ok(out)
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let tmp = format!("{path}.charon-tmp");
        // Stat first: the replacement gets the original's owner and mode back.
        let orig = self.sftp.metadata(path).await.ok();
        let mut f = self
            .sftp
            .open_with_flags(&tmp, OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
            .await?;
        f.write_all(data).await?;
        f.shutdown().await?;
        if let Some(a) = &orig {
            // Mode on the temp file too, so the new file is never briefly wider open.
            let _ = self.sftp.set_metadata(&tmp, mode_of(a)).await;
            // SFTP v3 rename refuses to clobber an existing file.
            let _ = self.sftp.remove_file(path).await;
        }
        self.sftp.rename(&tmp, path).await.with_context(|| format!("rename {tmp} over {path}"))?;
        if let Some(a) = &orig {
            // chown before chmod: chown clears setuid/setgid, chmod puts them back.
            // Only root (sudo-sftp) can give a file away; as the login user the file
            // is already ours, so restoring the same owner is a no-op that succeeds.
            if let (Some(uid), Some(gid)) = (a.uid, a.gid) {
                let mut own = FileAttributes::empty();
                own.uid = Some(uid);
                own.gid = Some(gid);
                if let Err(e) = self.sftp.set_metadata(path, own).await {
                    tracing::warn!(path, uid, gid, "write: could not restore owner: {e}");
                }
            }
            if let Err(e) = self.sftp.set_metadata(path, mode_of(a)).await {
                tracing::warn!(path, "write: could not restore mode: {e}");
            }
        }
        Ok(())
    }

    async fn download(&self, path: &str, local: &Path, t: &Transfer) -> Result<()> {
        let total = self.sftp.metadata(path).await?.size.unwrap_or(0);
        let mut src = self.sftp.open(path).await?;
        let mut dst = tokio::fs::File::create(local).await.with_context(|| format!("create {}", local.display()))?;
        let r = match pump(&mut src, &mut dst, total, t).await {
            Ok(_) => dst.flush().await.map_err(Into::into),
            Err(e) => Err(e),
        };
        if r.is_err() {
            let _ = tokio::fs::remove_file(local).await;
        }
        r.map(|_| ())
    }

    async fn upload(&self, local: &Path, path: &str, t: &Transfer) -> Result<()> {
        let mut src = tokio::fs::File::open(local).await.with_context(|| format!("open {}", local.display()))?;
        let total = src.metadata().await?.len();
        let mut dst = self
            .sftp
            .open_with_flags(path, OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
            .await?;
        // shutdown waits for every write ack and closes the handle; its error is the
        // upload's error (a failed write surfaces here, not in pump).
        let r = match pump(&mut src, &mut dst, total, t).await {
            Ok(_) => dst.shutdown().await.context("finish upload"),
            Err(e) => {
                let _ = dst.shutdown().await;
                Err(e)
            }
        };
        if r.is_err() {
            let _ = self.sftp.remove_file(path).await;
        }
        r.map(|_| ())
    }

    async fn copy_file(&self, from: &str, to: &str, t: &Transfer) -> Result<()> {
        let total = self.sftp.metadata(from).await?.size.unwrap_or(0);
        let mut src = self.sftp.open(from).await?;
        let mut dst = self
            .sftp
            .open_with_flags(to, OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
            .await?;
        let r = match pump(&mut src, &mut dst, total, t).await {
            Ok(_) => dst.shutdown().await.context("finish copy"),
            Err(e) => {
                let _ = dst.shutdown().await;
                Err(e)
            }
        };
        if r.is_err() {
            let _ = self.sftp.remove_file(to).await;
        }
        r.map(|_| ())
    }

    async fn set_times(&self, path: &str, mtime: u64) -> Result<()> {
        let mut a = FileAttributes::empty();
        a.mtime = Some(mtime as u32);
        a.atime = Some(mtime as u32);
        self.sftp.set_metadata(path, a).await?;
        Ok(())
    }

    fn join(&self, dir: &str, name: &str) -> String {
        posix_join(dir, name)
    }
}

// ---------------------------------------------------------------- local

pub struct LocalFs;

fn local_entry(path: &Path) -> Result<Entry> {
    let meta = std::fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    let kind = if ft.is_dir() {
        "dir"
    } else if ft.is_symlink() {
        "symlink"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    };
    let target_kind = if ft.is_symlink() {
        Some(match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => "dir",
            Ok(m) if m.is_file() => "file",
            _ => "other",
        })
    } else {
        None
    };
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    };
    #[cfg(not(unix))]
    let mode = 0;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    Ok(Entry {
        name: path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned()),
        path: path.to_string_lossy().into_owned(),
        kind,
        target_kind,
        size: meta.len(),
        mtime,
        mode,
        user: None,
        group: None,
    })
}

#[async_trait]
impl Fs for LocalFs {
    async fn home(&self) -> Result<String> {
        Ok(dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?.to_string_lossy().into_owned())
    }

    async fn list(&self, path: &str) -> Result<Listing> {
        let path = path.to_string();
        tokio::task::spawn_blocking(move || {
            let dir = std::fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(&path));
            let dir = strip_verbatim(dir);
            let mut entries = Vec::new();
            for e in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
                let e = e?;
                if let Ok(entry) = local_entry(&e.path()) {
                    entries.push(entry);
                }
            }
            let parent = dir.parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty());
            Ok(Listing { path: dir.to_string_lossy().into_owned(), parent, entries })
        })
        .await?
    }

    async fn stat(&self, path: &str) -> Result<Entry> {
        let path = PathBuf::from(path);
        tokio::task::spawn_blocking(move || local_entry(&path)).await?
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        Ok(tokio::fs::create_dir(path).await?)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        match tokio::fs::rename(from, to).await {
            Ok(()) => Ok(()),
            // Another drive / filesystem (Windows ERROR_NOT_SAME_DEVICE, Unix EXDEV):
            // copy, then remove the original.
            Err(e) if matches!(e.raw_os_error(), Some(17) if cfg!(windows)) || matches!(e.raw_os_error(), Some(18) if cfg!(unix)) => {
                let (from, to) = (PathBuf::from(from), PathBuf::from(to));
                tokio::task::spawn_blocking(move || -> Result<()> {
                    copy_local_tree(&from, &to)?;
                    remove_local_tree(&from)
                })
                .await?
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, path: &str, is_dir: bool) -> Result<()> {
        if is_dir {
            Ok(tokio::fs::remove_dir(path).await?)
        } else {
            Ok(tokio::fs::remove_file(path).await?)
        }
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            Ok(tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777)).await?)
        }
        #[cfg(not(unix))]
        {
            let _ = (path, mode);
            Err(anyhow!("chmod is not available on this platform"))
        }
    }

    async fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let size = tokio::fs::metadata(path).await?.len();
        if size > limit {
            return Err(anyhow!("{path} is {size} bytes; the editor limit is {limit}"));
        }
        Ok(tokio::fs::read(path).await?)
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let tmp = format!("{path}.charon-tmp");
        tokio::fs::write(&tmp, data).await.with_context(|| format!("write {tmp}"))?;
        #[cfg(unix)]
        if let Ok(m) = tokio::fs::metadata(path).await {
            let _ = tokio::fs::set_permissions(&tmp, m.permissions()).await;
        }
        tokio::fs::rename(&tmp, path).await.with_context(|| format!("rename {tmp} over {path}"))?;
        Ok(())
    }

    async fn download(&self, path: &str, local: &Path, t: &Transfer) -> Result<()> {
        let mut src = tokio::fs::File::open(path).await?;
        let total = src.metadata().await?.len();
        let mut dst = tokio::fs::File::create(local).await?;
        let r = match pump(&mut src, &mut dst, total, t).await {
            Ok(_) => dst.flush().await.map_err(Into::into),
            Err(e) => Err(e),
        };
        if r.is_err() {
            let _ = tokio::fs::remove_file(local).await;
        }
        r.map(|_| ())
    }

    async fn upload(&self, local: &Path, path: &str, t: &Transfer) -> Result<()> {
        self.download(&local.to_string_lossy(), Path::new(path), t).await
    }

    async fn copy_file(&self, from: &str, to: &str, t: &Transfer) -> Result<()> {
        self.download(from, Path::new(to), t).await
    }

    async fn remove_tree(&self, path: &str) -> Result<()> {
        let p = PathBuf::from(path);
        tokio::task::spawn_blocking(move || remove_local_tree(&p)).await?
    }

    async fn set_times(&self, path: &str, mtime: u64) -> Result<()> {
        let p = path.to_string();
        tokio::task::spawn_blocking(move || {
            filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(mtime as i64, 0))
        })
        .await??;
        Ok(())
    }

    fn join(&self, dir: &str, name: &str) -> String {
        Path::new(dir).join(name).to_string_lossy().into_owned()
    }
}

/// Delete a local file, symlink or directory tree (symlinks are not followed).
fn remove_local_tree(p: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(p).with_context(|| format!("stat {}", p.display()))?;
    if m.is_dir() {
        std::fs::remove_dir_all(p).with_context(|| format!("delete {}", p.display()))
    } else {
        std::fs::remove_file(p).with_context(|| format!("delete {}", p.display()))
    }
}

/// Copy a local file or directory tree to `to` (which must not exist yet).
fn copy_local_tree(from: &Path, to: &Path) -> Result<()> {
    if std::fs::symlink_metadata(from)?.is_dir() {
        std::fs::create_dir(to).with_context(|| format!("create {}", to.display()))?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_local_tree(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(from, to).with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- shell (after sudo)

/// Files through the interactive shell itself, for when the shell has become a
/// different user than the SFTP subsystem (sudo -i, su -). Every operation is a
/// hidden command (see `hidden.rs`); reads/writes go through base64, so this is
/// for browsing and editor-sized files — big transfers stay on SftpFs.
pub struct ShellFs {
    pub session: Arc<dyn crate::session::Session>,
}

/// Typed writes go through readline line by line: keep them small.
pub const SHELL_WRITE_LIMIT: u64 = 1024 * 1024;
/// Reads stream back as base64 over the pty.
pub const SHELL_READ_LIMIT: u64 = 32 * 1024 * 1024;

use crate::hidden::sq;

impl ShellFs {
    async fn run(&self, cmd: &str) -> Result<String> {
        let (out, code) = self.session.exec_hidden(cmd).await?;
        // The 133;C preexec marker fires inside the capture window; strip all escapes.
        let text = crate::hidden::strip_escapes(&String::from_utf8_lossy(&out)).replace("\r\n", "\n");
        if code != 0 {
            return Err(anyhow!("{}", text.trim().lines().last().unwrap_or("command failed")));
        }
        Ok(text)
    }
}

/// One `ls -la --time-style=+%s` / `stat -c '%A %h %U %G %s %Y %n'` line. `ls -l`
/// right-aligns columns with runs of spaces, so split on whitespace runs, not
/// single spaces; the name (7th field) keeps its own spaces and any ` -> target`.
pub fn parse_ls_line(line: &str, dir: &str) -> Option<Entry> {
    let mut rest = line;
    let mut fields = [""; 6]; // perms, links, user, group, size, mtime
    for f in fields.iter_mut() {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        *f = &rest[..end];
        rest = &rest[end..];
    }
    let [perms, _links, user, group, size_s, mtime_s] = fields;
    let size: u64 = size_s.parse().ok()?;
    let mtime: u64 = mtime_s.parse().ok()?;
    let rest = rest.trim_start();
    if perms.len() < 10 || rest.is_empty() {
        return None;
    }
    let (name, link) = match rest.split_once(" -> ") {
        Some((n, t)) => (n, Some(t)),
        None => (rest, None),
    };
    if name == "." || name == ".." {
        return None;
    }
    let kind = match &perms[..1] {
        "d" => "dir",
        "l" => "symlink",
        "-" => "file",
        _ => "other",
    };
    let _ = link;
    Some(Entry {
        name: name.to_string(),
        path: if name.starts_with('/') { name.to_string() } else { posix_join(dir, name) },
        kind,
        target_kind: None,
        size,
        mtime,
        mode: mode_from_perms(&perms[1..10]),
        user: Some(user.to_string()),
        group: Some(group.to_string()),
    })
}

/// `rwxr-sr-t` → 0o3755.
pub fn mode_from_perms(p: &str) -> u32 {
    let b = p.as_bytes();
    if b.len() < 9 {
        return 0;
    }
    let mut m = 0u32;
    for (i, &c) in b.iter().enumerate().take(9) {
        let bit = 1 << (8 - i);
        match c {
            b'r' | b'w' | b'x' => m |= bit,
            b's' | b't' => m |= bit,
            b'S' | b'T' => {}
            _ => {}
        }
    }
    if matches!(b[2], b's' | b'S') { m |= 0o4000; }
    if matches!(b[5], b's' | b'S') { m |= 0o2000; }
    if matches!(b[8], b't' | b'T') { m |= 0o1000; }
    m
}

#[async_trait]
impl Fs for ShellFs {
    async fn home(&self) -> Result<String> {
        Ok(self.run("printf %s \"$HOME\"").await?.trim().to_string())
    }

    async fn list(&self, path: &str) -> Result<Listing> {
        let canon = self.run(&format!("cd -- {} && pwd -P", sq(path))).await?.trim().to_string();
        // `command` + --color=never: root's login shell often aliases ls to --color=auto,
        // whose CSI codes would otherwise land mid-line.
        let out = self.run(&format!("command ls -la --color=never --time-style=+%s -- {} 2>&1", sq(&canon))).await?;
        let mut entries: Vec<Entry> = out.lines().filter_map(|l| parse_ls_line(l, &canon)).collect();
        // Symlink targets: one extra command for the lot.
        let links: Vec<String> = entries.iter().filter(|e| e.kind == "symlink").map(|e| e.path.clone()).collect();
        if !links.is_empty() {
            let cmd = format!("for f in {}; do if [ -d \"$f\" ]; then echo d; elif [ -f \"$f\" ]; then echo f; else echo o; fi; done", links.iter().map(|l| sq(l)).collect::<Vec<_>>().join(" "));
            let kinds: Vec<&'static str> = self.run(&cmd).await?.lines().map(|l| match l.trim() { "d" => "dir", "f" => "file", _ => "other" }).collect();
            let mut i = 0;
            for e in entries.iter_mut().filter(|e| e.kind == "symlink") {
                e.target_kind = Some(kinds.get(i).copied().unwrap_or("other"));
                i += 1;
            }
        }
        Ok(Listing { parent: posix_parent(&canon), path: canon, entries })
    }

    async fn stat(&self, path: &str) -> Result<Entry> {
        let out = self.run(&format!("command stat -c '%A %h %U %G %s %Y %n' -- {}", sq(path))).await?;
        let dir = posix_parent(path).unwrap_or_else(|| "/".into());
        parse_ls_line(out.trim(), &dir).ok_or_else(|| anyhow!("cannot parse stat output for {path}"))
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        self.run(&format!("mkdir -- {}", sq(path))).await.map(|_| ())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.run(&format!("mv -f -- {} {}", sq(from), sq(to))).await.map(|_| ())
    }

    async fn delete(&self, path: &str, is_dir: bool) -> Result<()> {
        let cmd = if is_dir { "rmdir" } else { "rm -f" };
        self.run(&format!("{cmd} -- {}", sq(path))).await.map(|_| ())
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.run(&format!("chmod {:o} -- {}", mode & 0o7777, sq(path))).await.map(|_| ())
    }

    async fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let e = self.stat(path).await?;
        let limit = limit.min(SHELL_READ_LIMIT);
        if e.size > limit {
            return Err(anyhow!("{path} is {} bytes; the shell backend limit is {limit}", e.size));
        }
        let b64 = self.run(&format!("base64 < {} | tr -d '\\n'", sq(path))).await?;
        use base64::Engine;
        Ok(base64::engine::general_purpose::STANDARD.decode(b64.trim())?)
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        if data.len() as u64 > SHELL_WRITE_LIMIT {
            return Err(anyhow!("{} bytes is too large to type through the shell (limit {SHELL_WRITE_LIMIT})", data.len()));
        }
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let body: Vec<&str> = b64.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect();
        self.run(&shell_write_cmd(path, &body.join("\n"))).await.map(|_| ())
    }

    async fn download(&self, path: &str, local: &Path, t: &Transfer) -> Result<()> {
        t.report(0, 0, "running", None);
        let bytes = self.read(path, SHELL_READ_LIMIT).await?;
        t.check()?;
        tokio::fs::write(local, &bytes).await.with_context(|| format!("write {}", local.display()))?;
        t.report(bytes.len() as u64, bytes.len() as u64, "running", None);
        Ok(())
    }

    async fn upload(&self, local: &Path, path: &str, t: &Transfer) -> Result<()> {
        let bytes = tokio::fs::read(local).await.with_context(|| format!("read {}", local.display()))?;
        t.report(0, bytes.len() as u64, "running", None);
        self.write(path, &bytes).await?;
        t.check()?;
        t.report(bytes.len() as u64, bytes.len() as u64, "running", None);
        Ok(())
    }

    async fn set_times(&self, path: &str, mtime: u64) -> Result<()> {
        self.run(&format!("touch -d @{mtime} -- {}", sq(path))).await.map(|_| ())
    }

    async fn copy_file(&self, from: &str, to: &str, t: &Transfer) -> Result<()> {
        t.report(0, 0, "running", None);
        self.run(&format!("cp -f -- {} {}", sq(from), sq(to))).await.map(|_| ())
    }

    async fn remove_tree(&self, path: &str) -> Result<()> {
        self.run(&format!("rm -rf -- {}", sq(path))).await.map(|_| ())
    }

    async fn download_dir_tar(&self, remote: &str, limit: u64) -> Result<Option<Vec<u8>>> {
        use base64::Engine;
        let parent = posix_parent(remote).unwrap_or_else(|| ".".into());
        let name = remote.rsplit('/').next().unwrap_or(remote).to_string();
        let size: u64 = self
            .run(&format!("du -sb -- {} 2>/dev/null | cut -f1", sq(remote)))
            .await
            .ok()
            .and_then(|o| o.trim().parse().ok())
            .unwrap_or(0);
        if size > limit {
            return Ok(None); // too big to buffer as base64 — fall back to per-file
        }
        let b64 = self
            .run(&format!("cd -- {} && tar -cf - -- {} | base64 | tr -d '
'", sq(&parent), sq(&name)))
            .await?;
        Ok(Some(base64::engine::general_purpose::STANDARD.decode(b64.trim())?))
    }

    fn join(&self, dir: &str, name: &str) -> String {
        posix_join(dir, name)
    }
}

/// `\\?\C:\x` → `C:\x` so paths stay readable in the UI.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

/// An in-memory sftp-server that behaves like Linux running as root: files it
/// creates are 0:0 0644, chown clears setuid/setgid, rename refuses to clobber
/// (SFTP v3). Every request is logged so tests can check the order of operations.
#[cfg(test)]
mod memsftp {
    use russh_sftp::protocol::{Attrs, Data, FileAttributes, Handle, OpenFlags, Status, StatusCode, Version};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, PartialEq)]
    pub struct MemFile { pub data: Vec<u8>, pub uid: u32, pub gid: u32, pub mode: u32 }

    #[derive(Default)]
    pub struct State {
        pub files: HashMap<String, MemFile>,
        pub log: Vec<String>,
        handles: HashMap<String, String>,
        /// Refuse every write at or past this offset (disk full), for error paths.
        pub fail_writes_from: Option<u64>,
    }

    impl State {
        pub fn handles_open(&self) -> usize { self.handles.len() }
    }

    pub struct Server(pub Arc<Mutex<State>>);

    fn ok(id: u32) -> Status {
        Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
    }
    fn attrs_of(f: &MemFile) -> FileAttributes {
        let mut a = FileAttributes::empty();
        a.size = Some(f.data.len() as u64);
        a.uid = Some(f.uid);
        a.gid = Some(f.gid);
        a.permissions = Some(0o100000 | f.mode);
        a
    }

    impl russh_sftp::server::Handler for Server {
        type Error = StatusCode;
        fn unimplemented(&self) -> Self::Error { StatusCode::OpUnsupported }
        async fn init(&mut self, _v: u32, _e: HashMap<String, String>) -> Result<Version, Self::Error> { Ok(Version::new()) }
        async fn open(&mut self, id: u32, path: String, flags: OpenFlags, _a: FileAttributes) -> Result<Handle, Self::Error> {
            let mut s = self.0.lock().unwrap();
            s.log.push(format!("open {path}"));
            if flags.contains(OpenFlags::CREATE) {
                let f = s.files.entry(path.clone()).or_insert(MemFile { data: vec![], uid: 0, gid: 0, mode: 0o644 });
                if flags.contains(OpenFlags::TRUNCATE) { f.data.clear(); }
            } else if !s.files.contains_key(&path) {
                return Err(StatusCode::NoSuchFile);
            }
            let h = format!("h{}", s.handles.len());
            s.handles.insert(h.clone(), path);
            Ok(Handle { id, handle: h })
        }
        async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
            let mut s = self.0.lock().unwrap();
            let path = s.handles.get(&handle).cloned().ok_or(StatusCode::Failure)?;
            if s.fail_writes_from.is_some_and(|at| offset >= at) { return Err(StatusCode::Failure); }
            let f = s.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
            let end = offset as usize + data.len();
            if f.data.len() < end { f.data.resize(end, 0); }
            f.data[offset as usize..end].copy_from_slice(&data);
            Ok(ok(id))
        }
        async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
            let s = self.0.lock().unwrap();
            let path = s.handles.get(&handle).ok_or(StatusCode::Failure)?;
            let f = s.files.get(path).ok_or(StatusCode::NoSuchFile)?;
            let start = (offset as usize).min(f.data.len());
            if start == f.data.len() { return Err(StatusCode::Eof); }
            let end = (start + len as usize).min(f.data.len());
            Ok(Data { id, data: f.data[start..end].to_vec() })
        }
        async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
            self.0.lock().unwrap().handles.remove(&handle);
            Ok(ok(id))
        }
        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let s = self.0.lock().unwrap();
            s.files.get(&path).map(|f| Attrs { id, attrs: attrs_of(f) }).ok_or(StatusCode::NoSuchFile)
        }
        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> { self.stat(id, path).await }
        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
            let path = self.0.lock().unwrap().handles.get(&handle).cloned().ok_or(StatusCode::Failure)?;
            self.stat(id, path).await
        }
        async fn setstat(&mut self, id: u32, path: String, a: FileAttributes) -> Result<Status, Self::Error> {
            let mut s = self.0.lock().unwrap();
            let f = s.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
            let mut what = Vec::new();
            if let Some(p) = a.permissions { f.mode = p & 0o7777; what.push(format!("chmod {:o}", f.mode)); }
            if let (Some(u), Some(g)) = (a.uid, a.gid) {
                f.uid = u; f.gid = g;
                f.mode &= !0o6000; // like chown(2): setuid/setgid dropped
                what.push(format!("chown {u}:{g}"));
            }
            let line = format!("setstat {path} {}", what.join(" "));
            s.log.push(line);
            Ok(ok(id))
        }
        async fn remove(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
            let mut s = self.0.lock().unwrap();
            s.log.push(format!("remove {path}"));
            s.files.remove(&path).map(|_| ok(id)).ok_or(StatusCode::NoSuchFile)
        }
        async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, Self::Error> {
            let mut s = self.0.lock().unwrap();
            s.log.push(format!("rename {from} {to}"));
            if s.files.contains_key(&to) { return Err(StatusCode::Failure); }
            let f = s.files.remove(&from).ok_or(StatusCode::NoSuchFile)?;
            s.files.insert(to, f);
            Ok(ok(id))
        }
    }

    /// An `SftpFs` talking to a fresh in-memory server holding `files`.
    pub async fn fs_with(files: Vec<(&str, MemFile)>) -> (super::SftpFs, Arc<Mutex<State>>) {
        let state = Arc::new(Mutex::new(State::default()));
        for (p, f) in files { state.lock().unwrap().files.insert(p.to_string(), f); }
        let (client, server) = tokio::io::duplex(64 * 1024);
        russh_sftp::server::run(server, Server(state.clone())).await;
        let sftp = russh_sftp::client::SftpSession::new(client).await.expect("handshake");
        (super::SftpFs::new(sftp), state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_as_sudo_preserves_owner_and_mode() {
        use memsftp::MemFile;
        // A root-run sftp-server (sudo-sftp) editing a setuid file owned by 1000:33.
        let orig = MemFile { data: b"old\n".to_vec(), uid: 1000, gid: 33, mode: 0o4750 };
        let (fs, state) = memsftp::fs_with(vec![("/etc/app.conf", orig.clone())]).await;
        fs.write("/etc/app.conf", b"new contents\n").await.expect("write");

        let s = state.lock().unwrap();
        let f = s.files.get("/etc/app.conf").expect("file still there");
        assert_eq!(f.data, b"new contents\n");
        assert_eq!((f.uid, f.gid), (1000, 33), "owner restored (the temp file was created 0:0)");
        assert_eq!(f.mode, 0o4750, "mode restored, setuid included");
        assert!(!s.files.contains_key("/etc/app.conf.charon-tmp"), "temp file renamed away");
        // Written to a temp file first, renamed over, then chown before chmod.
        let pos = |needle: &str| s.log.iter().position(|l| l.starts_with(needle)).unwrap_or_else(|| panic!("no {needle:?} in {:?}", s.log));
        assert!(pos("open /etc/app.conf.charon-tmp") < pos("rename /etc/app.conf.charon-tmp /etc/app.conf"));
        assert!(pos("rename") < pos("setstat /etc/app.conf chown 1000:33"));
        assert!(pos("setstat /etc/app.conf chown") < pos("setstat /etc/app.conf chmod 4750"));
    }

    #[tokio::test]
    async fn save_of_a_new_file_keeps_server_defaults() {
        let (fs, state) = memsftp::fs_with(vec![]).await;
        fs.write("/tmp/new.txt", b"x").await.expect("write");
        let s = state.lock().unwrap();
        let f = s.files.get("/tmp/new.txt").unwrap();
        assert_eq!((f.uid, f.gid, f.mode), (0, 0, 0o644));
        assert!(!s.log.iter().any(|l| l.starts_with("setstat")), "nothing to restore: {:?}", s.log);
    }

    #[tokio::test]
    async fn upload_waits_for_every_write_then_closes() {
        let (fs, state) = memsftp::fs_with(vec![]).await;
        let dir = std::env::temp_dir().join(format!("charon-up-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("big.bin");
        let body: Vec<u8> = (0..CHUNK * 3 + 17).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &body).unwrap();
        let t = Transfer::new("u1".into(), None, Arc::new(AtomicBool::new(false)));
        fs.upload(&src, "/srv/big.bin", &t).await.expect("upload");
        {
            let s = state.lock().unwrap();
            assert_eq!(s.files.get("/srv/big.bin").unwrap().data, body, "every chunk landed in order");
            assert_eq!(s.handles_open(), 0, "handle closed");
        }

        // A write the server refuses mid-file fails the upload (not swallowed by the
        // close) and the partial remote file is removed.
        state.lock().unwrap().fail_writes_from = Some(CHUNK as u64);
        let err = fs.upload(&src, "/srv/part.bin", &t).await.expect_err("server refused a write");
        assert!(!state.lock().unwrap().files.contains_key("/srv/part.bin"), "partial file removed: {err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_write_restores_owner_then_mode_after_the_rename() {
        let cmd = shell_write_cmd("/etc/it's", "Zm9v");
        let first = cmd.lines().next().unwrap();
        let at = |n: &str| first.find(n).unwrap_or_else(|| panic!("no {n:?} in {first}"));
        // stat before anything is written; mv before chown; chown before the final chmod.
        assert!(at("stat -c %u:%g -- '/etc/it'\\''s'") < at("base64 -d"));
        assert!(at("mv -f --") < at("chown \"$__o\" -- '/etc/it'\\''s'"));
        assert!(at("chown \"$__o\"") < first.rfind("chmod \"$__m\" -- '/etc/it'\\''s'").unwrap());
        // The heredoc body and terminator follow the one command line.
        assert_eq!(cmd.lines().skip(1).collect::<Vec<_>>(), vec!["Zm9v", "CHARON_B64"]);
    }

    #[test]
    fn posix_paths() {
        assert_eq!(posix_join("/home/admin", "x"), "/home/admin/x");
        assert_eq!(posix_join("/", "x"), "/x");
        assert_eq!(posix_parent("/home/admin"), Some("/home".into()));
        assert_eq!(posix_parent("/home"), Some("/".into()));
        assert_eq!(posix_parent("/"), None);
        assert_eq!(posix_parent("/home/admin/"), Some("/home".into()));
    }

    #[tokio::test]
    async fn local_fs_round_trip_with_progress_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let fs = LocalFs;
        let root = dir.path().to_string_lossy().into_owned();
        fs.mkdir(&fs.join(&root, "sub")).await.unwrap();
        let f = fs.join(&root, "a.txt");
        fs.write(&f, b"hello\n").await.unwrap();
        fs.write(&f, b"hello again\n").await.unwrap(); // atomic replace over an existing file
        assert_eq!(fs.read(&f, 1 << 20).await.unwrap(), b"hello again\n");
        assert!(fs.read(&f, 3).await.is_err(), "limit enforced");
        assert!(!Path::new(&format!("{f}.charon-tmp")).exists());

        let l = fs.list(&root).await.unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt") && names.contains(&"sub"));
        assert_eq!(l.entries.iter().find(|e| e.name == "sub").unwrap().kind, "dir");
        assert!(l.parent.is_some());

        // download = copy with progress; cancel flag stops it.
        let big = fs.join(&root, "big.bin");
        fs.write(&big, &vec![7u8; CHUNK * 3 + 10]).await.unwrap();
        let out = dir.path().join("copy.bin");
        let flag = Arc::new(AtomicBool::new(false));
        let t = Transfer::new("t1".into(), None, flag.clone());
        fs.download(&big, &out, &t).await.unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().len(), (CHUNK * 3 + 10) as u64);

        flag.store(true, Ordering::Relaxed);
        let out2 = dir.path().join("copy2.bin");
        let err = fs.download(&big, &out2, &t).await.unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(!out2.exists(), "partial file removed on cancel");

        fs.rename(&f, &fs.join(&root, "b.txt")).await.unwrap();
        fs.delete(&fs.join(&root, "b.txt"), false).await.unwrap();
        fs.delete(&fs.join(&root, "sub"), true).await.unwrap();
        assert!(fs.stat(&fs.join(&root, "b.txt")).await.is_err());
    }

    #[test]
    fn ls_lines_parse_into_entries() {
        let e = parse_ls_line("drwxr-xr-x 5 root root 4096 1700000000 .config", "/root").unwrap();
        assert_eq!((e.kind, e.path.as_str(), e.mode, e.mtime, e.user.as_deref()), ("dir", "/root/.config", 0o755, 1700000000, Some("root")));
        let e = parse_ls_line("-rw-r----- 1 admin adm 12 1700000001 my file.txt", "/home/admin").unwrap();
        assert_eq!((e.kind, e.name.as_str(), e.size, e.mode), ("file", "my file.txt", 12, 0o640));
        let e = parse_ls_line("lrwxrwxrwx 1 root root 7 1700000002 bin -> usr/bin", "/").unwrap();
        assert_eq!((e.kind, e.name.as_str(), e.path.as_str()), ("symlink", "bin", "/bin"));
        assert!(parse_ls_line("total 48", "/").is_none());
        assert!(parse_ls_line("drwxr-xr-x 5 root root 4096 1700000000 .", "/").is_none());
        // stat -c output names the full path
        let e = parse_ls_line("-rwsr-xr-x 1 root root 100 1 /usr/bin/sudo", "/usr/bin").unwrap();
        assert_eq!((e.path.as_str(), e.mode), ("/usr/bin/sudo", 0o4755));
        assert_eq!(mode_from_perms("rwxrwxrwt"), 0o1777);
        assert_eq!(mode_from_perms("rw-r-Sr--"), 0o2644);
    }

    #[test]
    fn colored_ls_output_parses_after_strip() {
        // Representative of `ls --color=auto -la` on a root login shell, with the
        // hook's 133;C preexec marker and SGR colour around the names.
        let raw = concat!(
            "\x1b]133;C\x07",
            "total 24\r\n",
            "drwxr-xr-x 4 root root 4096 1700000000 \x1b[01;34m.config\x1b[0m\r\n",
            "-rw-r----- 1 root root  33 1700000001 secret.txt\r\n",
            "lrwxrwxrwx 1 root root   9 1700000002 \x1b[01;36mlatest\x1b[0m -> \x1b[01;34mreleases/9\x1b[0m\r\n",
            "\x1b]133;C\x07",
        );
        let text = crate::hidden::strip_escapes(raw).replace("\r\n", "\n");
        let entries: Vec<Entry> = text.lines().filter_map(|l| parse_ls_line(l, "/root")).collect();
        assert_eq!(entries.len(), 3);
        assert_eq!((entries[0].name.as_str(), entries[0].kind, entries[0].mode), (".config", "dir", 0o755));
        assert_eq!((entries[1].name.as_str(), entries[1].kind, entries[1].size), ("secret.txt", "file", 33));
        assert_eq!((entries[2].name.as_str(), entries[2].kind), ("latest", "symlink"));
    }

    #[tokio::test]
    async fn sudo_sftp_sends_password_then_speaks_sftp() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (client, mut server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(async move {
            // 1. the sudo password line
            let mut line = Vec::new();
            loop {
                let mut b = [0u8; 1];
                server.read_exact(&mut b).await.unwrap();
                if b[0] == b'\n' {
                    break;
                }
                line.push(b[0]);
            }
            assert_eq!(line, b"s3cret");
            // 2. SSH_FXP_INIT: uint32 len, byte type=1, uint32 version
            let mut init = [0u8; 9];
            server.read_exact(&mut init).await.unwrap();
            assert_eq!(init[4], 1, "expected SSH_FXP_INIT");
            // 3. reply SSH_FXP_VERSION (v3, no extensions)
            server.write_all(&[0, 0, 0, 5, 2, 0, 0, 0, 3]).await.unwrap();
            server.flush().await.unwrap();
            // keep the pipe open so the client side stays usable
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
        let fs = sudo_sftp_over(client, "s3cret").await.expect("sftp handshake");
        drop(fs);
        srv.await.unwrap();
    }

    #[test]
    fn transfers_registry_cancels_by_id() {
        let t = Transfers::default();
        let f = t.start("x");
        assert!(!f.load(Ordering::Relaxed));
        assert!(t.cancel("x"));
        assert!(f.load(Ordering::Relaxed));
        t.finish("x");
        assert!(!t.cancel("x"));
    }
}
