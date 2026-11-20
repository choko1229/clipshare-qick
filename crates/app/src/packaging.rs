//! MSIX パッケージの操作（更新の適用・署名の検証・自分自身の削除）。
//!
//! 実際のインストール・削除は Windows（PackageManager / App Installer）が行う。ここでは依頼するだけ。

use std::path::Path;

use windows::Foundation::Uri;
use windows::Management::Deployment::{AddPackageOptions, PackageManager};
use windows::core::HSTRING;

/// `ERROR_PACKAGES_IN_USE`（エクスプローラーが右クリック拡張を読み込んでいる場合など）。
const ERROR_PACKAGES_IN_USE: i32 = 0x8007_3D02_u32 as i32;

/// 更新の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// 適用した（アプリは Windows によって終了・再起動される）。
    Applied,
    /// 使用中のため、次に使われていない時（再起動・次回起動時）に適用される。
    Deferred,
}

#[derive(Debug, thiserror::Error)]
pub enum PackagingError {
    #[error("パッケージを適用できませんでした（0x{0:08X}）")]
    Deploy(u32),
    #[error("署名を確認できませんでした（0x{0:08X}）")]
    Signature(u32),
    #[error("ファイルのパスが不正です")]
    Path,
}

/// ダウンロードしたパッケージの Authenticode 署名を検証する（信頼されていない・改ざんされていれば失敗）。
pub fn verify_signature(path: &Path) -> Result<(), PackagingError> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE, WinVerifyTrust,
    };
    let wide = HSTRING::from(path.as_os_str());
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: windows::core::PCWSTR(wide.as_ptr()),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        // 失効確認はネットワーク状況に左右されるため行わない（ハッシュの一致確認と併用する）。
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: data と file はこの関数内で有効。検証後に CLOSE で状態を解放する。
    let status = unsafe {
        WinVerifyTrust(
            HWND(std::ptr::null_mut()),
            &mut action,
            std::ptr::from_mut(&mut data).cast(),
        )
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: 同じ data で状態を解放する。
    let _ = unsafe {
        WinVerifyTrust(
            HWND(std::ptr::null_mut()),
            &mut action,
            std::ptr::from_mut(&mut data).cast(),
        )
    };
    if status == 0 {
        tracing::info!("更新ファイルの署名を確認しました");
        Ok(())
    } else {
        tracing::warn!(
            status = format!("0x{:08X}", status as u32),
            "更新ファイルの署名を確認できませんでした"
        );
        Err(PackagingError::Signature(status as u32))
    }
}

/// パッケージを適用する。`force` ならアプリを終了させて今すぐ、そうでなければ使用中なら次回に回す。
pub fn install_update(path: &Path, force: bool) -> Result<InstallOutcome, PackagingError> {
    let uri = Uri::CreateUri(&HSTRING::from(format!(
        "file:///{}",
        path.display().to_string().replace('\\', "/")
    )))
    .map_err(|_| PackagingError::Path)?;
    let pm = PackageManager::new().map_err(|e| PackagingError::Deploy(e.code().0 as u32))?;
    let add = |force_shutdown: bool, defer: bool| -> windows::core::Result<()> {
        let opts = AddPackageOptions::new()?;
        opts.SetForceAppShutdown(force_shutdown)?;
        opts.SetDeferRegistrationWhenPackagesAreInUse(defer)?;
        pm.AddPackageByUriAsync(&uri, &opts)?.get().map(|_| ())
    };
    match add(force, !force) {
        Ok(()) => Ok(if force {
            InstallOutcome::Applied
        } else {
            InstallOutcome::Deferred
        }),
        Err(e) if e.code().0 == ERROR_PACKAGES_IN_USE => {
            // 右クリック拡張がエクスプローラーに読み込まれていて置き換えられない。再起動時に適用させる。
            tracing::warn!("使用中のため、再起動後に更新されるよう予約します");
            add(false, true)
                .map(|()| InstallOutcome::Deferred)
                .map_err(|e| PackagingError::Deploy(e.code().0 as u32))
        }
        Err(e) => Err(PackagingError::Deploy(e.code().0 as u32)),
    }
}

/// 更新後に Windows がアプリを起動し直すよう登録する。
pub fn register_restart() {
    use windows::Win32::System::Recovery::{
        REGISTER_APPLICATION_RESTART_FLAGS, RegisterApplicationRestart,
    };
    let args = HSTRING::from("--background");
    // SAFETY: 引数は NUL 終端のワイド文字列。
    let r = unsafe { RegisterApplicationRestart(&args, REGISTER_APPLICATION_RESTART_FLAGS(0)) };
    if r.is_err() {
        tracing::warn!("再起動の登録に失敗しました");
    }
}

fn package_full_name() -> Option<String> {
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    // SAFETY: まず長さを問い合わせ、確保したバッファに書かせる。
    unsafe {
        let _ = GetCurrentPackageFullName(&mut len, None);
        if len == 0 || len > 1024 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        let r = GetCurrentPackageFullName(&mut len, Some(windows::core::PWSTR(buf.as_mut_ptr())));
        if r.0 != 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

/// このプロセスの終了後に、自分のパッケージを削除するよう依頼する。
pub fn schedule_self_removal() -> bool {
    let Some(name) = package_full_name() else {
        return false;
    };
    // パッケージ名は英数字・ピリオド・アンダースコア・ハイフンのみ（念のため検証してからコマンドに入れる）。
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return false;
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let script = format!("Start-Sleep -Seconds 3; Remove-AppxPackage -Package '{name}'");
    let r = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn();
    match r {
        Ok(_) => {
            tracing::info!("パッケージの削除を依頼しました");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "パッケージの削除を依頼できませんでした");
            false
        }
    }
}

/// 実行エイリアス（MSIX の `uap5:AppExecutionAlias`）。
fn alias_exe() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|l| {
        std::path::PathBuf::from(l)
            .join("Microsoft")
            .join("WindowsApps")
            .join("clipshare-quick.exe")
    })
}

/// インストールしたアプリを起動する（初回設定へ進む）。
pub fn launch_installed_app() {
    if let Some(exe) = alias_exe() {
        if let Err(e) = std::process::Command::new(exe).spawn() {
            tracing::warn!(error = %e, "アプリを起動できませんでした");
        }
    }
}

/// エクスプローラーを再起動する（右クリックメニューを読み込み直すため。利用者がボタンを押したときだけ）。
pub fn restart_explorer() {
    let killed = std::process::Command::new("taskkill.exe")
        .args(["/f", "/im", "explorer.exe"])
        .status();
    if killed.is_ok() {
        let _ = std::process::Command::new("explorer.exe").spawn();
    }
}

/// `.cer` から発行者と拇印（SHA-1）を読む。
pub fn certificate_info(cer: &Path) -> Option<crate::setup::SignerInfo> {
    use windows::Win32::Security::Cryptography::{
        CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_SHA1_HASH_PROP_ID, CertCreateCertificateContext,
        CertFreeCertificateContext, CertGetCertificateContextProperty, CertGetNameStringW,
        PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
    };
    let bytes = std::fs::read(cer).ok()?;
    // SAFETY: bytes は呼び出しの間有効。成功時の証明書コンテキストは最後に解放する。
    unsafe {
        let ctx = CertCreateCertificateContext(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, &bytes);
        if ctx.is_null() {
            return None;
        }
        let mut name = [0u16; 256];
        let n = CertGetNameStringW(ctx, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut name))
            as usize;
        let publisher = String::from_utf16_lossy(&name[..n.saturating_sub(1).min(name.len())]);
        let mut hash = [0u8; 20];
        let mut len = hash.len() as u32;
        let ok = CertGetCertificateContextProperty(
            ctx,
            CERT_SHA1_HASH_PROP_ID,
            Some(hash.as_mut_ptr().cast()),
            &mut len,
        )
        .is_ok();
        let _ = CertFreeCertificateContext(Some(ctx));
        if !ok {
            return None;
        }
        let fingerprint = hash
            .iter()
            .take(len as usize)
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        Some(crate::setup::SignerInfo {
            publisher,
            fingerprint,
        })
    }
}

/// 自己署名の証明書を「信頼されたユーザー」に追加する（管理者の確認が出る。利用者が明示的に選んだときだけ）。
pub fn trust_certificate(cer: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let path = cer.display().to_string();
    if path.contains('"') {
        return Err("CERT_PATH_INVALID".into());
    }
    let params = HSTRING::from(format!("-addstore TrustedPeople \"{path}\""));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: windows::core::w!("runas"),
        lpFile: windows::core::w!("certutil.exe"),
        lpParameters: windows::core::PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: info の文字列は呼び出しの間有効。プロセスハンドルは待ってから閉じる。
    unsafe {
        ShellExecuteExW(&mut info).map_err(|_| "CERT_TRUST_CANCELLED".to_owned())?;
        if info.hProcess.is_invalid() {
            return Err("CERT_TRUST_FAILED".into());
        }
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if code == 0 {
            Ok(())
        } else {
            Err(format!("CERT_TRUST_FAILED (0x{code:08X})"))
        }
    }
}

/// パッケージをインストールする（進捗は 0〜100）。
pub fn install_package(
    path: &Path,
    progress: impl Fn(f32) + Send + 'static,
) -> Result<(), PackagingError> {
    use windows::Management::Deployment::{DeploymentProgress, DeploymentResult};
    use windows_future::AsyncOperationProgressHandler;
    let uri = Uri::CreateUri(&HSTRING::from(format!(
        "file:///{}",
        path.display().to_string().replace('\\', "/")
    )))
    .map_err(|_| PackagingError::Path)?;
    let pm = PackageManager::new().map_err(|e| PackagingError::Deploy(e.code().0 as u32))?;
    let opts = AddPackageOptions::new().map_err(|e| PackagingError::Deploy(e.code().0 as u32))?;
    let op = pm
        .AddPackageByUriAsync(&uri, &opts)
        .map_err(|e| PackagingError::Deploy(e.code().0 as u32))?;
    let handler =
        AsyncOperationProgressHandler::<DeploymentResult, DeploymentProgress>::new(move |_, p| {
            progress(p.percentage as f32);
            Ok(())
        });
    let _ = op.SetProgress(&handler);
    op.get()
        .map(|_| ())
        .map_err(|e| PackagingError::Deploy(e.code().0 as u32))
}

/// デスクトップにショートカットを作る（実行エイリアスを指す）。
pub fn create_desktop_shortcut() -> Result<(), String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, IPersistFile,
    };
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
    };
    use windows::core::Interface;
    let target = alias_exe().ok_or("LOCALAPPDATA")?;
    // SAFETY: このスレッドで COM を初期化し、各インターフェースはスコープ内で解放される。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let desktop = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None)
            .map_err(|e| e.to_string())?;
        let dir = desktop.to_string().map_err(|e| e.to_string())?;
        CoTaskMemFree(Some(desktop.0 as *const _));
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        link.SetPath(&HSTRING::from(target.as_os_str()))
            .map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let lnk = std::path::PathBuf::from(dir).join("Clipshare Quick.lnk");
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
