//! アプリが使うローカルのパス。

use std::path::{Path, PathBuf};

/// データ保存先の一覧。テストでは任意のルートを渡せる。
#[derive(Debug, Clone)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    /// 既定の保存先（`%LOCALAPPDATA%\ClipshareQuick`）。
    ///
    /// MSIX ではマニフェストでファイル書き込みの仮想化を無効にしているため、
    /// エクスプローラー拡張（パッケージ外のプロセス）からも同じ場所が見える。
    pub fn default_root() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("ClipshareQuick"))
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    pub fn history_file(&self) -> PathBuf {
        self.root.join("history.json")
    }
    pub fn processed_file(&self) -> PathBuf {
        self.root.join("processed.json")
    }
    pub fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }
    pub fn update_cache_file(&self) -> PathBuf {
        self.root.join("update.json")
    }
    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn download_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }
    /// エクスプローラー拡張が読むフラグ（右クリックメニューの表示可否）。
    pub fn shell_flag_file(&self) -> PathBuf {
        self.root.join("contextmenu.disabled")
    }
}
