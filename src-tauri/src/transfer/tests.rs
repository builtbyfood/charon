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

// ---------------------------------------------------------------- copy / move / delete (LocalFs)

fn local_tree() -> (tempfile::TempDir, Arc<dyn Fs>, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src/proj/deep")).unwrap();
    std::fs::write(root.join("src/proj/a.txt"), "alpha").unwrap();
    std::fs::write(root.join("src/proj/deep/b.txt"), "beta").unwrap();
    std::fs::create_dir_all(root.join("dest")).unwrap();
    let base = root.to_string_lossy().into_owned();
    (dir, Arc::new(crate::sftp::LocalFs), base)
}

#[test]
fn is_within_matches_self_and_descendants_only() {
    assert!(is_within("/a/b", "/a/b"));
    assert!(is_within("/a/b", "/a/b/c"));
    assert!(is_within("/a/b/", "/a/b/c"));
    assert!(is_within(r"C:\x\y", r"C:\x\y\z"));
    assert!(!is_within("/a/b", "/a/bc"));
    assert!(!is_within("/a/b", "/a"));
}

#[tokio::test]
async fn copy_tree_copies_a_folder_and_keeps_both_in_place() {
    let (dir, fs, base) = local_tree();
    let src = fs.join(&fs.join(&base, "src"), "proj");
    let dest = fs.join(&base, "dest");
    let (report, log) = collector();
    copy_tree(fs.clone(), &src, &dest, &TreeOpts::default(), Arc::new(AtomicBool::new(false)), report, None).await.unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("dest/proj/a.txt")).unwrap(), "alpha");
    assert_eq!(std::fs::read_to_string(dir.path().join("dest/proj/deep/b.txt")).unwrap(), "beta");
    assert!(dir.path().join("src/proj/a.txt").exists(), "source untouched");
    assert_eq!(done_of(&log.lock().unwrap()).0, 2);

    // Same directory: a duplicate beside it, whatever the policy says.
    let (report, _) = collector();
    let opts = TreeOpts { conflict: Conflict::Overwrite, ..TreeOpts::default() };
    copy_tree(fs.clone(), &src, &fs.join(&base, "src"), &opts, Arc::new(AtomicBool::new(false)), report, None).await.unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("src/proj (2)/deep/b.txt")).unwrap(), "beta");
}

#[tokio::test]
async fn copy_tree_refuses_a_folder_into_itself() {
    let (_dir, fs, base) = local_tree();
    let src = fs.join(&fs.join(&base, "src"), "proj");
    let (report, _) = collector();
    let r = copy_tree(fs.clone(), &src, &fs.join(&src, "deep"), &TreeOpts::default(), Arc::new(AtomicBool::new(false)), report, None).await;
    assert!(r.is_err());
}

#[tokio::test]
async fn move_item_renames_and_honours_conflicts() {
    let (dir, fs, base) = local_tree();
    let a = fs.join(&fs.join(&fs.join(&base, "src"), "proj"), "a.txt");
    let dest = fs.join(&base, "dest");
    std::fs::write(dir.path().join("dest/a.txt"), "old").unwrap();

    // skip leaves both alone
    assert_eq!(move_item(fs.as_ref(), &a, &dest, Conflict::Skip).await.unwrap(), None);
    assert!(dir.path().join("src/proj/a.txt").exists());
    // keep both
    let to = move_item(fs.as_ref(), &a, &dest, Conflict::Rename).await.unwrap().unwrap();
    assert!(to.ends_with("a (2).txt"));
    assert!(!dir.path().join("src/proj/a.txt").exists());
    // overwrite replaces the target
    let to2 = move_item(fs.as_ref(), &to, &fs.join(&base, "src"), Conflict::Overwrite).await.unwrap().unwrap();
    std::fs::write(dir.path().join("src/a.txt"), "new").unwrap();
    let back = move_item(fs.as_ref(), &to2, &dest, Conflict::Overwrite).await.unwrap().unwrap();
    assert!(back.ends_with("a (2).txt"));

    // a folder never moves into itself; moving to where it already is is a no-op
    let proj = fs.join(&fs.join(&base, "src"), "proj");
    assert!(move_item(fs.as_ref(), &proj, &fs.join(&proj, "deep"), Conflict::Overwrite).await.is_err());
    assert_eq!(move_item(fs.as_ref(), &proj, &fs.join(&base, "src"), Conflict::Overwrite).await.unwrap(), Some(proj.clone()));
    // a whole folder moves
    move_item(fs.as_ref(), &proj, &dest, Conflict::Overwrite).await.unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("dest/proj/deep/b.txt")).unwrap(), "beta");
}

#[tokio::test]
async fn remove_tree_deletes_a_whole_folder() {
    let (dir, fs, base) = local_tree();
    fs.remove_tree(&fs.join(&base, "src")).await.unwrap();
    assert!(!dir.path().join("src").exists());
    assert!(dir.path().join("dest").exists());
}

#[tokio::test]
async fn default_remove_tree_walks_children_first() {
    // The trait default (used by SFTP) is exercised through a wrapper that only
    // forwards the primitives, so LocalFs's remove_dir_all is not what runs.
    struct Prim(crate::sftp::LocalFs);
    #[async_trait]
    impl Fs for Prim {
        async fn home(&self) -> anyhow::Result<String> { self.0.home().await }
        async fn list(&self, p: &str) -> anyhow::Result<Listing> { self.0.list(p).await }
        async fn stat(&self, p: &str) -> anyhow::Result<Entry> { self.0.stat(p).await }
        async fn mkdir(&self, p: &str) -> anyhow::Result<()> { self.0.mkdir(p).await }
        async fn rename(&self, a: &str, b: &str) -> anyhow::Result<()> { self.0.rename(a, b).await }
        async fn delete(&self, p: &str, d: bool) -> anyhow::Result<()> { self.0.delete(p, d).await }
        async fn chmod(&self, p: &str, m: u32) -> anyhow::Result<()> { self.0.chmod(p, m).await }
        async fn read(&self, p: &str, l: u64) -> anyhow::Result<Vec<u8>> { self.0.read(p, l).await }
        async fn write(&self, p: &str, d: &[u8]) -> anyhow::Result<()> { self.0.write(p, d).await }
        async fn download(&self, p: &str, l: &Path, t: &Transfer) -> anyhow::Result<()> { self.0.download(p, l, t).await }
        async fn upload(&self, l: &Path, p: &str, t: &Transfer) -> anyhow::Result<()> { self.0.upload(l, p, t).await }
        fn join(&self, d: &str, n: &str) -> String { self.0.join(d, n) }
    }
    let (dir, _, base) = local_tree();
    let fs = Prim(crate::sftp::LocalFs);
    // and the default copy_file (through a temp file)
    let t = Transfer::new("t".into(), None, Arc::new(AtomicBool::new(false)));
    let a = fs.join(&fs.join(&fs.join(&base, "src"), "proj"), "a.txt");
    fs.copy_file(&a, &fs.join(&fs.join(&base, "dest"), "a.txt"), &t).await.unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("dest/a.txt")).unwrap(), "alpha");
    fs.remove_tree(&fs.join(&base, "src")).await.unwrap();
    assert!(!dir.path().join("src").exists());
}
