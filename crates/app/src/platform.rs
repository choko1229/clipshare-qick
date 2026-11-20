//! Windows 固有の小さな機能（テーマ取得、URL・フォルダを開く、パッケージ判定、スタートアップ登録、
//! フォルダ選択ダイアログ、メインスレッドの起床）。

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, SW_SHOWNORMAL, WM_APP};
use windows::core::{HSTRING, PCWSTR, w};

/// メインスレッドを起こすメッセージ。
pub const WM_WAKE: u32 = WM_APP + 1;

/// 開発中に OS の設定（スタートアップ・通知の登録）を変更しないための環境変数。
pub const DEV_NO_SYSTEM_CHANGES: &str = "CQ_DEV_NO_SYSTEM_CHANGES";

pub fn system_changes_disabled() -> bool {
    std::env::var_os(DEV_NO_SYSTEM_CHANGES).is_some_and(|v| v == "1")
}

/// Windows のアプリ モードがライトか（読めなければダーク）。
pub fn apps_use_light_theme() -> bool {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: 出力先は u32 で、サイズを正しく渡している。
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut value).cast()),
            Some(&mut size),
        )
    };
    r.is_ok() && value == 1
}

/// URL を既定のブラウザで開く（https のみ。開発用にループバックの http も許可）。
pub fn open_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let loopback = matches!(u.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
    if !(u.scheme() == "https" || (u.scheme() == "http" && loopback)) {
        tracing::warn!("開けないURLのため無視しました");
        return false;
    }
    shell_open(&HSTRING::from(u.as_str()))
}

/// フォルダやファイルをエクスプローラーで開く。
pub fn open_path(p: &Path) -> bool {
    shell_open(&HSTRING::from(p.as_os_str()))
}

fn shell_open(target: &HSTRING) -> bool {
    // SAFETY: 文字列は呼び出しの間有効。戻り値が 32 より大きければ成功。
    let r = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            target,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    r.0 as isize > 32
}

/// MSIX パッケージとして動いているか。
pub fn is_packaged() -> bool {
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    // SAFETY: 長さだけを問い合わせる（バッファなし）。
    let r = unsafe { GetCurrentPackageFullName(&mut len, None) };
    r == ERROR_INSUFFICIENT_BUFFER
}

/// メインスレッドへ起床メッセージを送る。
pub fn wake_thread(thread_id: u32) {
    // SAFETY: スレッド ID は起動時に取得した UI スレッド。失敗しても害はない。
    let _ = unsafe { PostThreadMessageW(thread_id, WM_WAKE, WPARAM(0), LPARAM(0)) };
}

pub fn current_thread_id() -> u32 {
    // SAFETY: 引数なしの API。
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("ClipshareQuick");
/// MSIX マニフェストの StartupTask の TaskId。
const STARTUP_TASK_ID: &str = "ClipshareQuickStartup";

/// Windows 起動時に開始するか設定する。パッケージ版は StartupTask、開発版は HKCU の Run キー。
pub fn set_launch_at_startup(enabled: bool) -> Result<(), String> {
    if system_changes_disabled() {
        tracing::info!(enabled, "開発モードのためスタートアップ登録を変更しません");
        return Ok(());
    }
    if is_packaged() {
        return set_startup_task(enabled);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut key = HKEY::default();
    // SAFETY: 出力先の HKEY は成功時に有効になり、下で閉じる。
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    }
    .ok()
    .map_err(|e| e.to_string())?;
    let r = if enabled {
        let cmd = format!("\"{}\" --background", exe.display());
        let wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
        // SAFETY: REG_SZ として NUL 終端のワイド文字列をバイト列で渡す。
        unsafe {
            RegSetValueExW(
                key,
                RUN_VALUE,
                None,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    wide.as_ptr().cast::<u8>(),
                    wide.len() * 2,
                )),
            )
        }
        .ok()
    } else {
        // SAFETY: 値が無い場合のエラーは無視する。
        let _ = unsafe { RegDeleteValueW(key, RUN_VALUE) };
        Ok(())
    };
    // SAFETY: 開いたキーを閉じる。
    let _ = unsafe { RegCloseKey(key) };
    r.map_err(|e| e.to_string())?;
    tracing::info!(enabled, "スタートアップ登録を変更しました");
    Ok(())
}

fn set_startup_task(enabled: bool) -> Result<(), String> {
    use windows::ApplicationModel::{StartupTask, StartupTaskState};
    let task = StartupTask::GetAsync(&HSTRING::from(STARTUP_TASK_ID))
        .and_then(|op| op.get())
        .map_err(|e| format!("StartupTask: {e}"))?;
    if enabled {
        let state = task
            .RequestEnableAsync()
            .and_then(|op| op.get())
            .map_err(|e| e.to_string())?;
        if state != StartupTaskState::Enabled && state != StartupTaskState::EnabledByPolicy {
            tracing::warn!(
                ?state,
                "スタートアップを有効にできませんでした（タスクマネージャーで無効化されている可能性）"
            );
        }
    } else {
        task.Disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// フォルダを選ぶダイアログ（モーダル）。キャンセルなら `None`。
pub fn pick_folder(initial: Option<&Path>) -> Option<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
        SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    // SAFETY: COM は UI スレッドで初期化済み（winit が OLE を初期化する）。各インターフェースはスコープ内で解放される。
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        if let Some(dir) = initial.filter(|d| d.is_dir()) {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from(dir.as_os_str()),
                None,
            ) {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(None::<HWND>).ok()?;
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
        s.map(PathBuf::from)
    }
}
