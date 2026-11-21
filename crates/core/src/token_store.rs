//! トークン保管（Windows 資格情報マネージャー）。
//!
//! トークンは資格情報マネージャーにのみ保存する。平文ファイル・設定・ログ・環境変数には置かない。
//! 取得できない・壊れている場合は「未保存」と同じ扱いにし、UI から再認可を促す。

use std::sync::Mutex;

use crate::secret::SecretString;

/// 資格情報の対象名の接頭辞。
pub const TARGET_PREFIX: &str = "ClipshareQuick:token:";
/// 資格情報マネージャーに保存できるデータの上限（`CRED_MAX_CREDENTIAL_BLOB_SIZE`）。
const MAX_BLOB: usize = 5 * 512;

#[derive(Debug, thiserror::Error)]
pub enum TokenStoreError {
    #[error("資格情報マネージャーを操作できませんでした（{0}）")]
    Os(String),
    #[error("保存されている資格情報が壊れています")]
    Corrupted,
    #[error("トークンが長すぎます")]
    TooLarge,
}

/// トークンの保存先。
pub trait TokenStore: Send + Sync + std::fmt::Debug {
    fn save(&self, token: &SecretString) -> Result<(), TokenStoreError>;
    fn load(&self) -> Result<Option<SecretString>, TokenStoreError>;
    fn delete(&self) -> Result<(), TokenStoreError>;
}

/// API のホストごとに対象名を分ける（開発用モックと本番の混同を防ぐ）。
pub fn target_name_for(api_base: &url::Url) -> String {
    let host = api_base.host_str().unwrap_or("unknown");
    match api_base.port() {
        Some(p) => format!("{TARGET_PREFIX}{host}:{p}"),
        None => format!("{TARGET_PREFIX}{host}"),
    }
}

/// メモリ上の保存先（テスト・資格情報マネージャーが使えない環境用）。
#[derive(Debug, Default)]
pub struct MemoryTokenStore {
    inner: Mutex<Option<SecretString>>,
}

impl TokenStore for MemoryTokenStore {
    fn save(&self, token: &SecretString) -> Result<(), TokenStoreError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| TokenStoreError::Os("lock".to_owned()))?;
        *g = Some(token.clone());
        Ok(())
    }
    fn load(&self) -> Result<Option<SecretString>, TokenStoreError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| TokenStoreError::Os("lock".to_owned()))?;
        Ok(g.clone())
    }
    fn delete(&self) -> Result<(), TokenStoreError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| TokenStoreError::Os("lock".to_owned()))?;
        *g = None;
        Ok(())
    }
}

fn decode_blob(bytes: &[u8]) -> Result<SecretString, TokenStoreError> {
    let s = std::str::from_utf8(bytes).map_err(|_| TokenStoreError::Corrupted)?;
    let ok = !s.is_empty() && s.len() <= MAX_BLOB && s.chars().all(|c| c.is_ascii_graphic());
    if ok {
        Ok(SecretString::new(s))
    } else {
        Err(TokenStoreError::Corrupted)
    }
}

#[cfg(windows)]
pub use win::CredentialManagerStore;

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME};
    use windows::Win32::Security::Credentials::{
        CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW,
        CredFree, CredReadW, CredWriteW,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};
    use zeroize::Zeroize;

    /// Windows 資格情報マネージャーに保存する。
    #[derive(Debug, Clone)]
    pub struct CredentialManagerStore {
        target: String,
    }

    impl CredentialManagerStore {
        pub fn new(target: impl Into<String>) -> Self {
            Self {
                target: target.into(),
            }
        }
    }

    fn os_err(e: &windows::core::Error) -> TokenStoreError {
        TokenStoreError::Os(format!("0x{:08X}", e.code().0))
    }

    impl TokenStore for CredentialManagerStore {
        fn save(&self, token: &SecretString) -> Result<(), TokenStoreError> {
            let mut blob: Vec<u8> = token.expose().as_bytes().to_vec();
            if blob.len() > MAX_BLOB {
                blob.zeroize();
                return Err(TokenStoreError::TooLarge);
            }
            let mut target: Vec<u16> = self.target.encode_utf16().chain(Some(0)).collect();
            let mut user: Vec<u16> = "Clipshare Quick".encode_utf16().chain(Some(0)).collect();
            let cred = CREDENTIALW {
                Flags: CRED_FLAGS(0),
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(target.as_mut_ptr()),
                Comment: PWSTR::null(),
                LastWritten: FILETIME::default(),
                CredentialBlobSize: u32::try_from(blob.len())
                    .map_err(|_| TokenStoreError::TooLarge)?,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount: 0,
                Attributes: std::ptr::null_mut(),
                TargetAlias: PWSTR::null(),
                UserName: PWSTR(user.as_mut_ptr()),
            };
            // SAFETY: `cred` の各ポインタは、この呼び出しの間だけ有効なローカルのバッファを指している。
            let r = unsafe { CredWriteW(&cred, 0) };
            blob.zeroize();
            r.map_err(|e| os_err(&e))?;
            tracing::info!("トークンを資格情報マネージャーに保存しました");
            Ok(())
        }

        fn load(&self) -> Result<Option<SecretString>, TokenStoreError> {
            let target = HSTRING::from(self.target.as_str());
            let mut out: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `out` は成功時に OS が確保した CREDENTIALW を指す。下で CredFree する。
            let r =
                unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut out) };
            match r {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => return Ok(None),
                Err(e) => return Err(os_err(&e)),
            }
            if out.is_null() {
                return Ok(None);
            }
            // SAFETY: CredReadW が成功したので `out` は有効。blob は CredentialBlobSize バイト。
            let result = unsafe {
                let c = &*out;
                if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                    Err(TokenStoreError::Corrupted)
                } else {
                    let bytes =
                        std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
                    decode_blob(bytes)
                }
            };
            // SAFETY: CredReadW が確保したメモリを解放する（1 回だけ）。
            unsafe { CredFree(out.cast()) };
            result.map(Some)
        }

        fn delete(&self) -> Result<(), TokenStoreError> {
            let target = HSTRING::from(self.target.as_str());
            // SAFETY: 対象名は有効な NUL 終端文字列。
            let r = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) };
            match r {
                Ok(()) => {
                    tracing::info!("資格情報マネージャーのトークンを削除しました");
                    Ok(())
                }
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
                Err(e) => Err(os_err(&e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryTokenStore::default();
        assert!(s.load().unwrap().is_none());
        s.save(&SecretString::new("t")).unwrap();
        assert_eq!(s.load().unwrap().unwrap().expose(), "t");
        s.delete().unwrap();
        assert!(s.load().unwrap().is_none());
    }

    #[test]
    fn target_name_includes_host_and_port() {
        let u = url::Url::parse("https://clipshare.link").unwrap();
        assert_eq!(target_name_for(&u), "ClipshareQuick:token:clipshare.link");
        let u = url::Url::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(target_name_for(&u), "ClipshareQuick:token:127.0.0.1:8080");
    }

    #[test]
    fn corrupted_blob_is_detected() {
        assert!(decode_blob(&[0xff, 0xfe]).is_err());
        assert!(decode_blob(b"").is_err());
        assert!(decode_blob(b"has space").is_err());
        assert!(decode_blob(b"ok-token_1").is_ok());
    }

    /// 実際の資格情報マネージャーを使う（テスト専用の対象名で、終了時に削除する）。
    #[cfg(windows)]
    #[test]
    fn credential_manager_roundtrip() {
        let target = format!("{TARGET_PREFIX}test-{}", std::process::id());
        let s = CredentialManagerStore::new(&target);
        s.delete().unwrap();
        assert!(s.load().unwrap().is_none());
        s.save(&SecretString::new("cq_test_token_123")).unwrap();
        assert_eq!(s.load().unwrap().unwrap().expose(), "cq_test_token_123");
        s.save(&SecretString::new("cq_test_token_456")).unwrap();
        assert_eq!(s.load().unwrap().unwrap().expose(), "cq_test_token_456");
        s.delete().unwrap();
        assert!(s.load().unwrap().is_none());
        assert!(
            s.save(&SecretString::new("x".repeat(MAX_BLOB + 1)))
                .is_err()
        );
    }
}
