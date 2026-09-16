//! Where Charon keeps its data. Portable mode wins if `portable.flag` sits beside the exe.

use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub portable: bool,
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn detect() -> Result<Self> {
        let exe = std::env::current_exe().context("current_exe")?;
        let exe_dir = exe.parent().context("exe has no parent dir")?.to_path_buf();

        let (portable, data_dir) = if exe_dir.join("portable.flag").exists() {
            (true, exe_dir.join("data"))
        } else {
            let base = dirs::config_dir().context("no config dir on this platform")?;
            (false, base.join("charon"))
        };

        std::fs::create_dir_all(&data_dir).context("create data dir")?;
        std::fs::create_dir_all(data_dir.join("profiles"))?;
        Ok(Self { portable, data_dir })
    }

    /// Where the embedded browser keeps its profile (cache, storage), when it
    /// should not be the platform default: beside the data in portable mode (so a
    /// portable Charon leaves nothing in %LOCALAPPDATA%), and a separate
    /// `webview-dev` for debug builds so a dev loop never shares a profile with
    /// an installed or portable exe. `None` = leave it to Tauri.
    pub fn webview_data_dir(&self) -> Option<PathBuf> {
        if self.portable {
            Some(self.data_dir.join("webview"))
        } else if cfg!(debug_assertions) {
            Some(self.data_dir.join("webview-dev"))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_keeps_the_webview_profile_beside_the_data() {
        let p = AppPaths { portable: true, data_dir: PathBuf::from("X:/charon/data") };
        assert_eq!(p.webview_data_dir(), Some(PathBuf::from("X:/charon/data").join("webview")));
        let installed = AppPaths { portable: false, data_dir: PathBuf::from("X:/cfg/charon") };
        assert_eq!(installed.webview_data_dir().is_some(), cfg!(debug_assertions));
    }
}
