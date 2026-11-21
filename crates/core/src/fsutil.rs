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

/// データフォルダのアクセス権を「現在の利用者・SYSTEM・Administrators」だけに絞る（継承しない）。
///
/// 履歴には匿名の削除用トークン、設定には利用者のフォルダ構成が入る。`%LOCALAPPDATA%` から継承する
/// アクセス権は環境によって他の主体（アプリコンテナーなど）にも読み取りを許していることがあるため、
/// 明示的な DACL を設定する。既存のファイルにも継承で反映される。
#[cfg(windows)]
pub fn restrict_dir_to_current_user(dir: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };
    use windows::core::{BOOL, HSTRING, PCWSTR};

    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let sid = cq_ipc::current_user_sid().map_err(|e| e.to_string())?;
    if !sid.starts_with("S-1-")
        || !sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
    {
        return Err("SID".to_owned());
    }
    // OICI = フォルダ配下のファイル・フォルダへ継承。FA = フル コントロール。SY = SYSTEM、BA = Administrators。
    let sddl = HSTRING::from(format!(
        "D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
    ));
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: SDDL は NUL 終端。成功時の記述子は最後に LocalFree する。
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    let mut present = BOOL(0);
    let mut defaulted = BOOL(0);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: sd は直前に作った有効な記述子。dacl はその内部を指す（sd の解放まで有効）。
    let got = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
    let path = HSTRING::from(dir.as_os_str());
    let result = match got {
        Ok(()) if present.as_bool() && !dacl.is_null() => {
            // SAFETY: path は NUL 終端、dacl は sd の生存中は有効。所有者・グループ・SACL は変更しない。
            let r = unsafe {
                SetNamedSecurityInfoW(
                    PCWSTR(path.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(dacl.cast_const()),
                    None,
                )
            };
            if r.0 == 0 {
                Ok(())
            } else {
                Err(format!("SetNamedSecurityInfoW: {}", r.0))
            }
        }
        Ok(()) => Err("DACL".to_owned()),
        Err(e) => Err(e.to_string()),
    };
    // SAFETY: ConvertStringSecurityDescriptorToSecurityDescriptorW が確保した記述子を 1 回だけ解放する。
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    result
}

#[cfg(not(windows))]
pub fn restrict_dir_to_current_user(_dir: &Path) -> Result<(), String> {
    Ok(())
}

/// 前回の異常終了で残った書き込み途中の一時ファイル（`.{名前}.{pid}.tmp`）を消す。
/// 秘密値（匿名の削除用トークンなど）を含みうるため、読み込み時に片付ける。
pub fn remove_stale_tmp(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = format!(".{}.", name.to_string_lossy());
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with(&prefix) && n.ends_with(".tmp") {
            match std::fs::remove_file(e.path()) {
                Ok(()) => tracing::info!("書き込み途中の一時ファイルを削除しました"),
                Err(err) => tracing::warn!(error = %err, "一時ファイルを削除できませんでした"),
            }
        }
    }
}

/// 壊れたファイルを退避せずに削除する（秘密値を含みうるファイル用）。
pub fn discard(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::warn!(file = %crate::logging::mask_path(path), "壊れたファイルを削除しました")
        }
        Err(e) => tracing::warn!(error = %e, "壊れたファイルを削除できませんでした"),
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

    #[cfg(windows)]
    #[test]
    fn data_dir_acl_is_restricted_and_still_usable() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("ClipshareQuick");
        restrict_dir_to_current_user(&data).unwrap();
        // 自分は読み書きできる（継承した ACE で配下のファイルにも効く）
        let p = data.join("history.json");
        save_json(&p, &vec![1]).unwrap();
        let v: Option<Vec<i32>> = load_json(&p).unwrap();
        assert_eq!(v, Some(vec![1]));
        // icacls で確認: 継承の無効（保護）と 3 主体のみ
        let out = std::process::Command::new("icacls")
            .arg(&data)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(!text.contains("(I)"), "継承した ACE が残っていない: {text}");
        assert_eq!(
            text.lines().filter(|l| l.contains(":(")).count(),
            3,
            "{text}"
        );
    }

    #[test]
    fn stale_tmp_files_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("history.json");
        std::fs::write(dir.path().join(".history.json.1234.tmp"), b"secret").unwrap();
        std::fs::write(dir.path().join(".other.json.1.tmp"), b"x").unwrap();
        remove_stale_tmp(&p);
        assert!(!dir.path().join(".history.json.1234.tmp").exists());
        assert!(dir.path().join(".other.json.1.tmp").exists());
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
