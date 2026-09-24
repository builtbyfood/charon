//! Tests for the recursive transfer engine, driven by an in-memory `Fs` mock so
//! they run without a network or a real SFTP server.

use super::*;
use crate::sftp::{Entry, Fs, Listing, Transfer};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// One node in the mock remote filesystem.
#[derive(Clone)]
enum Node {
    Dir,
    File { data: Vec<u8>, mode: u32, mtime: u64 },
    Symlink { target_dir: bool },
}

/// In-memory remote filesystem. Paths are POSIX ("/a/b"); the map holds every
/// node and dirs are reconstructed from path prefixes.
struct MockFs {
    nodes: HashMap<String, Node>,
    /// Downloads succeed until this many have started, then `cancel` is tripped
    /// and the current (and later) downloads fail as cancelled. 0 = never.
    cancel_after: usize,
    cancel_flag: Mutex<Option<Arc<AtomicBool>>>,
    started: AtomicUsize,
    /// Records the remote paths actually downloaded (data written locally).
    downloaded: Mutex<Vec<String>>,
}

impl MockFs {
    fn new(nodes: Vec<(&str, Node)>) -> Arc<Self> {
        Arc::new(Self {
            nodes: nodes.into_iter().map(|(p, n)| (p.to_string(), n)).collect(),
            cancel_after: 0,
            cancel_flag: Mutex::new(None),
            started: AtomicUsize::new(0),
            downloaded: Mutex::new(Vec::new()),
        })
    }

    fn children(&self, dir: &str) -> Vec<(String, Node)> {
        let prefix = if dir == "/" { "/".to_string() } else { format!("{dir}/") };
        let mut out = Vec::new();
        for (path, node) in &self.nodes {
            if let Some(rest) = path.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    out.push((path.clone(), node.clone()));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn entry_for(&self, path: &str, node: &Node) -> Entry {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let (kind, target_kind, size, mode, mtime) = match node {
            Node::Dir => ("dir", None, 0, 0o755, 0),
            Node::File { data, mode, mtime } => ("file", None, data.len() as u64, *mode, *mtime),
            Node::Symlink { target_dir } => ("symlink", Some(if *target_dir { "dir" } else { "file" }), 0, 0o777, 0),
        };
        Entry {
            name,
            path: path.to_string(),
            kind,
            target_kind,
            size,
            mtime,
            mode,
            user: None,
            group: None,
        }
    }
}

#[async_trait]
impl Fs for MockFs {
    async fn home(&self) -> anyhow::Result<String> {
        Ok("/".into())
    }

    async fn list(&self, path: &str) -> anyhow::Result<Listing> {
        if !matches!(self.nodes.get(path), Some(Node::Dir)) && path != "/" {
            anyhow::bail!("not a directory: {path}");
        }
        let entries = self.children(path).iter().map(|(p, n)| self.entry_for(p, n)).collect();
        Ok(Listing { path: path.to_string(), parent: None, entries })
    }

    async fn stat(&self, path: &str) -> anyhow::Result<Entry> {
        let node = self.nodes.get(path).ok_or_else(|| anyhow::anyhow!("no such path: {path}"))?;
        Ok(self.entry_for(path, node))
    }

    async fn mkdir(&self, _path: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn rename(&self, _from: &str, _to: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _path: &str, _is_dir: bool) -> anyhow::Result<()> {
        Ok(())
    }
    async fn chmod(&self, _path: &str, _mode: u32) -> anyhow::Result<()> {
        Ok(())
    }
    async fn read(&self, _path: &str, _limit: u64) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("unused")
    }
    async fn write(&self, _path: &str, _data: &[u8]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn download(&self, path: &str, local: &Path, t: &Transfer) -> anyhow::Result<()> {
        // Trip cancellation once enough downloads have started.
        let n = self.started.fetch_add(1, Ordering::SeqCst) + 1;
        if self.cancel_after != 0 && n >= self.cancel_after {
            if let Some(flag) = self.cancel_flag.lock().unwrap().as_ref() {
                flag.store(true, Ordering::SeqCst);
            }
        }
        if t.cancelled() {
            anyhow::bail!("cancelled");
        }
        let data = match self.nodes.get(path) {
            Some(Node::File { data, .. }) => data.clone(),
            _ => anyhow::bail!("not a file: {path}"),
        };
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(local, &data)?;
        self.downloaded.lock().unwrap().push(path.to_string());
        t.report(data.len() as u64, data.len() as u64, "done", None);
        Ok(())
    }

    async fn upload(&self, _local: &Path, _path: &str, _t: &Transfer) -> anyhow::Result<()> {
        Ok(())
    }

    fn join(&self, dir: &str, name: &str) -> String {
        if dir.ends_with('/') {
            format!("{dir}{name}")
        } else {
            format!("{dir}/{name}")
        }
    }
}

fn file(data: &str) -> Node {
    Node::File { data: data.as_bytes().to_vec(), mode: 0o644, mtime: 1_600_000_000 }
}

/// Collect every reported event into a shared Vec.
fn collector() -> (Reporter, Arc<Mutex<Vec<TreeEvent>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let reporter: Reporter = Arc::new(move |e| sink.lock().unwrap().push(e));
    (reporter, log)
}

fn plan_of(log: &[TreeEvent]) -> Vec<PlanFile> {
    log.iter()
        .find_map(|e| if let TreeEvent::Plan { files, .. } = e { Some(files.clone()) } else { None })
        .expect("a Plan event")
}

fn done_of(log: &[TreeEvent]) -> (u64, u64, Vec<String>, bool) {
    log.iter()
        .find_map(|e| {
            if let TreeEvent::Done { files_done, files_total, failed, cancelled, .. } = e {
                Some((*files_done, *files_total, failed.clone(), *cancelled))
            } else {
                None
            }
        })
        .expect("a Done event")
}

#[tokio::test]
async fn recursive_download_builds_the_local_tree() {
    let fs = MockFs::new(vec![
        ("/proj", Node::Dir),
        ("/proj/a.txt", file("aaa")),
        ("/proj/sub", Node::Dir),
        ("/proj/sub/b.txt", file("bbbb")),
        ("/proj/sub/deep", Node::Dir),
        ("/proj/sub/deep/c.txt", file("ccccc")),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let (report, log) = collector();

    download_tree(fs.clone(), "/proj", tmp.path(), &TreeOpts::default(), Arc::new(AtomicBool::new(false)), report, None)
        .await
        .unwrap();

    let log = log.lock().unwrap();
    let mut rels: Vec<_> = plan_of(&log).iter().map(|f| f.rel.clone()).collect();
    rels.sort();
    assert_eq!(rels, vec!["proj/a.txt", "proj/sub/b.txt", "proj/sub/deep/c.txt"]);

    // Files exist locally with the right contents and the directory shape.
    assert_eq!(std::fs::read_to_string(tmp.path().join("proj/a.txt")).unwrap(), "aaa");
    assert_eq!(std::fs::read_to_string(tmp.path().join("proj/sub/b.txt")).unwrap(), "bbbb");
    assert_eq!(std::fs::read_to_string(tmp.path().join("proj/sub/deep/c.txt")).unwrap(), "ccccc");

    let (files_done, files_total, failed, cancelled) = done_of(&log);
    assert_eq!(files_done, 3);
    assert_eq!(files_total, 3);
    assert!(failed.is_empty());
    assert!(!cancelled);
}

#[tokio::test]
async fn cancel_mid_tree_stops_early() {
    let mut nodes = vec![("/big", Node::Dir)];
    let contents: Vec<String> = (0..10).map(|i| format!("data-{i}")).collect();
    for (i, c) in contents.iter().enumerate() {
        nodes.push((Box::leak(format!("/big/f{i:02}.txt").into_boxed_str()), file(c)));
    }
    let mut fs = MockFs::new(nodes);
    // Cancel once the 3rd download starts; parallel=1 makes it deterministic.
    Arc::get_mut(&mut fs).unwrap().cancel_after = 3;
    let cancel = Arc::new(AtomicBool::new(false));
    *fs.cancel_flag.lock().unwrap() = Some(cancel.clone());

    let tmp = tempfile::tempdir().unwrap();
    let (report, log) = collector();
    let opts = TreeOpts { parallel: 1, ..TreeOpts::default() };

    download_tree(fs.clone(), "/big", tmp.path(), &opts, cancel.clone(), report, None).await.unwrap();

    let log = log.lock().unwrap();
    let (files_done, _total, _failed, cancelled) = done_of(&log);
    assert!(cancelled, "the tree should report cancellation");
    assert!(files_done < 10, "not every file should have transferred, got {files_done}");
    // Some files did transfer before the cancel; not all did.
    assert!(fs.downloaded.lock().unwrap().len() < 10);
}

#[tokio::test]
async fn conflict_skip_leaves_existing_untouched() {
    let fs = MockFs::new(vec![("/proj", Node::Dir), ("/proj/a.txt", file("new"))]);
    let tmp = tempfile::tempdir().unwrap();
    // Pre-existing destination directory with different content.
    std::fs::create_dir_all(tmp.path().join("proj")).unwrap();
    std::fs::write(tmp.path().join("proj/a.txt"), "OLD").unwrap();

    let (report, log) = collector();
    let opts = TreeOpts { conflict: Conflict::Skip, ..TreeOpts::default() };
    download_tree(fs.clone(), "/proj", tmp.path(), &opts, Arc::new(AtomicBool::new(false)), report, None)
        .await
        .unwrap();

    // Nothing overwritten; a Warn and an empty Done were reported.
    assert_eq!(std::fs::read_to_string(tmp.path().join("proj/a.txt")).unwrap(), "OLD");
    let log = log.lock().unwrap();
    assert!(log.iter().any(|e| matches!(e, TreeEvent::Warn { .. })));
    let (files_done, _total, _failed, _cancelled) = done_of(&log);
    assert_eq!(files_done, 0);
}

#[tokio::test]
async fn conflict_rename_writes_beside_existing() {
    let fs = MockFs::new(vec![("/proj", Node::Dir), ("/proj/a.txt", file("new"))]);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("proj")).unwrap();
    std::fs::write(tmp.path().join("proj/a.txt"), "OLD").unwrap();

    let (report, log) = collector();
    let opts = TreeOpts { conflict: Conflict::Rename, ..TreeOpts::default() };
    download_tree(fs.clone(), "/proj", tmp.path(), &opts, Arc::new(AtomicBool::new(false)), report, None)
        .await
        .unwrap();

    // Original untouched; the new tree landed under "proj (2)".
    assert_eq!(std::fs::read_to_string(tmp.path().join("proj/a.txt")).unwrap(), "OLD");
    assert_eq!(std::fs::read_to_string(tmp.path().join("proj (2)/a.txt")).unwrap(), "new");
    let log = log.lock().unwrap();
    assert_eq!(plan_of(&log)[0].rel, "proj (2)/a.txt");
}

#[tokio::test]
async fn symlinks_skipped_when_not_following() {
    let fs = MockFs::new(vec![
        ("/proj", Node::Dir),
        ("/proj/a.txt", file("aaa")),
        ("/proj/link", Node::Symlink { target_dir: false }),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let (report, log) = collector();
    let opts = TreeOpts { follow_symlinks: false, ..TreeOpts::default() };
    download_tree(fs.clone(), "/proj", tmp.path(), &opts, Arc::new(AtomicBool::new(false)), report, None)
        .await
        .unwrap();

    let log = log.lock().unwrap();
    let rels: Vec<_> = plan_of(&log).iter().map(|f| f.rel.clone()).collect();
    assert_eq!(rels, vec!["proj/a.txt"]); // the symlink is not in the plan
    assert!(log.iter().any(|e| matches!(e, TreeEvent::Warn { message } if message.contains("symlink skipped"))));
}
