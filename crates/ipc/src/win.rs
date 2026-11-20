//! Windows 固有部分（SID、パイプのセキュリティ記述子、同期クライアント）。

use std::ffi::c_void;
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::{IpcError, Request, Response, decode, encode, read_line_limited};

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

fn os(e: &windows::core::Error) -> IpcError {
    IpcError::Os(e.code().0)
}

/// 所有する HANDLE（ドロップ時に閉じる）。
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: 自分で開いたハンドルを 1 回だけ閉じる。
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// プロセスのトークンから利用者の SID 文字列を得る。
fn user_sid_of_process(process: HANDLE) -> Result<String, IpcError> {
    let mut token = HANDLE::default();
    // SAFETY: `token` は成功時に有効なハンドルになり、OwnedHandle が閉じる。
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(|e| os(&e))?;
    let token = OwnedHandle(token);
    let mut len = 0u32;
    // SAFETY: 1 回目は必要なサイズを得るだけ（失敗は想定どおり）。
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut len) };
    if len == 0 || len > 64 * 1024 {
        return Err(IpcError::Os(0));
    }
    // u64 で確保して TOKEN_USER の整列を満たす。
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` は `len` バイト以上あり、関数はそこへ TOKEN_USER を書く。
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            len,
            &mut len,
        )
    }
    .map_err(|e| os(&e))?;
    // SAFETY: 直前の呼び出しが成功したので先頭は有効な TOKEN_USER。
    let sid: PSID = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_to_string(sid)
}

fn sid_to_string(sid: PSID) -> Result<String, IpcError> {
    let mut out = PWSTR::null();
    // SAFETY: `sid` は有効な SID。成功時 `out` は LocalAlloc されたワイド文字列。
    unsafe { ConvertSidToStringSidW(sid, &mut out) }.map_err(|e| os(&e))?;
    // SAFETY: `out` は NUL 終端の文字列。読み終えたら LocalFree する。
    let s = unsafe { out.to_string() }.map_err(|_| IpcError::Malformed);
    // SAFETY: ConvertSidToStringSidW が確保したメモリを 1 回だけ解放する。
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out.0.cast())));
    }
    s
}

/// 現在のプロセスの利用者 SID（例: `S-1-5-21-...`）。
pub fn current_user_sid() -> Result<String, IpcError> {
    // SAFETY: GetCurrentProcess は擬似ハンドルを返し、閉じる必要はない。
    user_sid_of_process(unsafe { GetCurrentProcess() })
}

/// 利用者ごとのパイプ名。
pub fn pipe_name(sid: &str) -> String {
    format!(r"\\.\pipe\ClipshareQuick-{sid}")
}

/// 同じ利用者と SYSTEM だけに許可するセキュリティ記述子。
#[derive(Debug)]
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

// SAFETY: 中身は OS が確保した読み取り専用の記述子で、作成後は変更しない。
unsafe impl Send for PipeSecurity {}
// SAFETY: 同上（共有しても読み取りのみ）。
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    pub fn for_user(sid: &str) -> Result<Self, IpcError> {
        if !sid.starts_with("S-1-")
            || !sid
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == 'S')
        {
            return Err(IpcError::Malformed);
        }
        // D:P = 継承しない DACL。GA = 全アクセス。SY = SYSTEM。
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: SDDL は有効な NUL 終端文字列。成功時 `descriptor` は LocalAlloc された記述子。
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|e| os(&e))?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(24),
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    /// `CreateNamedPipeW` / tokio の `create_with_security_attributes_raw` に渡すポインタ。
    /// `self` が生きている間だけ有効。
    pub fn as_ptr(&self) -> *mut c_void {
        std::ptr::from_ref(&self.attributes).cast_mut().cast()
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: ConvertStringSecurityDescriptorToSecurityDescriptorW が確保した記述子を 1 回だけ解放する。
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

/// 接続したパイプのサーバーが自分と同じ利用者のプロセスか確認する。
fn verify_server(pipe: &std::fs::File, my_sid: &str) -> Result<(), IpcError> {
    let handle = HANDLE(pipe.as_raw_handle());
    let mut pid = 0u32;
    // SAFETY: `handle` は開いているパイプのハンドル。
    unsafe { GetNamedPipeServerProcessId(handle, &mut pid) }.map_err(|e| os(&e))?;
    // SAFETY: 成功時は有効なプロセスハンドル。OwnedHandle が閉じる。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| os(&e))?;
    let process = OwnedHandle(process);
    let server_sid = user_sid_of_process(process.0)?;
    if server_sid == my_sid {
        Ok(())
    } else {
        Err(IpcError::UntrustedPeer)
    }
}

/// パイプに接続する。サーバーが無ければ [`IpcError::NotRunning`]。
fn connect(name: &str, timeout: Duration) -> Result<std::fs::File, IpcError> {
    let deadline = Instant::now() + timeout;
    let wide = HSTRING::from(name);
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
        {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                return Err(IpcError::NotRunning);
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                if Instant::now() >= deadline {
                    return Err(IpcError::Io(e));
                }
                // SAFETY: パイプ名は有効な NUL 終端文字列。
                let _ = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 200) };
            }
            Err(e) => return Err(IpcError::Io(e)),
        }
    }
}

/// 要求を送り、応答を待つ（同期・ブロッキング）。呼び出し側はエクスプローラーの UI スレッドで呼ばないこと。
pub fn send_request(request: &Request, connect_timeout: Duration) -> Result<Response, IpcError> {
    let sid = current_user_sid()?;
    let mut pipe = connect(&pipe_name(&sid), connect_timeout)?;
    verify_server(&pipe, &sid)?;
    pipe.write_all(&encode(request)?)?;
    pipe.flush()?;
    let line = read_line_limited(&mut pipe)?;
    decode(&line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_and_security_descriptor() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"));
        assert!(pipe_name(&sid).starts_with(r"\\.\pipe\ClipshareQuick-S-1-"));
        let sec = PipeSecurity::for_user(&sid).unwrap();
        assert!(!sec.as_ptr().is_null());
        assert!(PipeSecurity::for_user("S-1-5;(A;;GA;;;WD)").is_err());
        assert!(PipeSecurity::for_user("garbage").is_err());
    }

    #[test]
    fn not_running_is_reported() {
        let r = connect(
            r"\\.\pipe\ClipshareQuick-test-does-not-exist",
            Duration::from_millis(100),
        );
        assert!(matches!(r, Err(IpcError::NotRunning)));
    }
}
