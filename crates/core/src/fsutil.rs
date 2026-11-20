//! ファイル操作の補助（原子的な書き込みなど）。

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::StorageError;

/// 一時ファイルへ書いてから置き換えることで、途中で落ちても壊れたファイルを残さない。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let dir = path
        .parent()
        .ok_or_else(|| StorageError::io(path, std::io::Error::other("親フォルダがありません")))?;
    std::fs::create_dir_all(dir).map_err(|e| StorageError::io(dir, e))?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".to_owned());
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| StorageError::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| StorageError::io(&tmp, e))?;
        f.sync_all().map_err(|e| StorageError::io(&tmp, e))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StorageError::io(path, e)
    })
}

/// JSON として原子的に保存する。
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(StorageError::Serialize)?;
    write_atomic(path, &bytes)
}

/// JSON を読み込む。ファイルが無ければ `Ok(None)`。
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, StorageError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| StorageError::Parse {
                    path: path.to_path_buf(),
                    source,
                })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StorageError::io(path, e)),
    }
}

/// 壊れたファイルを `.broken` に退避する（復旧不能なときに初期値で起動するため）。
pub fn quarantine(path: &Path) {
    let broken = path.with_extension("broken.json");
    if let Err(e) = std::fs::rename(path, &broken) {
        tracing::warn!(error = %e, "壊れたファイルを退避できませんでした");
    } else {
        tracing::warn!(file = %crate::logging::mask_path(path), "壊れたファイルを退避しました");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("a.json");
        save_json(&p, &vec![1, 2, 3]).unwrap();
        let v: Option<Vec<i32>> = load_json(&p).unwrap();
        assert_eq!(v, Some(vec![1, 2, 3]));
        save_json(&p, &vec![4]).unwrap();
        let v: Option<Vec<i32>> = load_json(&p).unwrap();
        assert_eq!(v, Some(vec![4]));
        // 一時ファイルが残っていない
        let leftovers: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn missing_is_none_and_broken_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.json");
        let v: Option<Vec<i32>> = load_json(&p).unwrap();
        assert!(v.is_none());
        std::fs::write(&p, b"{not json").unwrap();
        let r: Result<Option<Vec<i32>>, _> = load_json(&p);
        assert!(matches!(r, Err(StorageError::Parse { .. })));
        quarantine(&p);
        assert!(!p.exists());
    }
}
