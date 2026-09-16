//! Docs sidebar backend: refresh the bundled tldr pages into the data dir so the
//! app stays offline-capable but updatable. The build ships `public/tldr.json.gz`;
//! `tldr_update` downloads the latest tldr-pages archive and writes a fresh
//! `data/tldr.json.gz`, which the UI prefers over the bundled copy.

use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

const TLDR_ZIP: &str = "https://github.com/tldr-pages/tldr/releases/latest/download/tldr.zip";
/// The build-time bundle, embedded so the frontend can load it over IPC (the
/// webview CSP blocks a plain fetch of a static asset).
const BUNDLED: &[u8] = include_bytes!("../../public/tldr.json.gz");
const PLATFORMS: [&str; 3] = ["common", "linux", "windows"];

pub fn bundle_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("tldr.json.gz")
}

/// The gzipped tldr bundle to use: the updated copy in the data dir if present,
/// else the one embedded at build time.
pub fn bundle_bytes(data_dir: &Path) -> Vec<u8> {
    std::fs::read(bundle_path(data_dir)).unwrap_or_else(|_| BUNDLED.to_vec())
}

/// Download + extract English common/linux/windows pages, write data/tldr.json.gz.
/// Returns the number of pages written.
pub async fn update(data_dir: &Path) -> Result<usize> {
    let bytes = reqwest::get(TLDR_ZIP).await.context("download tldr.zip")?.bytes().await.context("read tldr.zip")?;
    // zip is sync; do the CPU work off the async runtime.
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || build(&bytes, &data_dir)).await?
}

fn build(zip_bytes: &[u8], data_dir: &Path) -> Result<usize> {
    let reader = std::io::Cursor::new(zip_bytes);
    let mut zip = zip::ZipArchive::new(reader).context("open zip")?;
    // linux/windows override common; collect by page name.
    let mut pages: BTreeMap<String, String> = BTreeMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = f.name().to_string();
        // Expect `pages/<platform>/<cmd>.md`.
        let Some(rest) = name.strip_prefix("pages/") else { continue };
        let mut parts = rest.splitn(2, '/');
        let (Some(platform), Some(file)) = (parts.next(), parts.next()) else { continue };
        if !PLATFORMS.contains(&platform) || !file.ends_with(".md") {
            continue;
        }
        let cmd = file.trim_end_matches(".md").to_string();
        if platform == "common" && pages.contains_key(&cmd) {
            continue; // don't override a platform-specific page with common
        }
        let mut md = String::new();
        f.read_to_string(&mut md)?;
        pages.insert(cmd, md);
    }
    if pages.is_empty() {
        anyhow::bail!("no pages found in the archive");
    }
    let json = serde_json::to_vec(&pages)?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&json)?;
    let gz = enc.finish()?;
    std::fs::write(bundle_path(data_dir), gz).context("write tldr.json.gz")?;
    Ok(pages.len())
}
