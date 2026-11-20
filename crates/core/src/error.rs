//! コア共通のエラー型。

use std::path::PathBuf;

/// 設定・履歴などローカル保存まわりのエラー。
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("ファイルの読み書きに失敗しました: {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ファイルの形式が不正です: {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("データを保存用に変換できませんでした")]
    Serialize(#[source] serde_json::Error),
}

impl StorageError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
