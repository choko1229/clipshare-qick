//! 画面・トレイ・通知から共通で使う操作と、設定変更に伴う OS 側の反映。

use std::sync::Arc;

use cq_core::engine::AccountView;
use cq_core::settings::Settings;

use crate::app::Shared;

/// 自動アップロードを切り替える。
pub fn set_auto_upload(shared: &Arc<Shared>, on: bool) -> Result<(), String> {
    shared.engine.set_auto_upload(on).map_err(|e| e.to_string())
}

/// サーバーから削除する（非同期）。
pub fn delete_entry(shared: &Arc<Shared>, id: String) {
    let s = Arc::clone(shared);
    shared.rt.spawn(async move {
        if let Err(e) = s.engine.delete_remote(&id).await {
            tracing::warn!(error = %e, "削除できませんでした");
            crate::toast::show_simple(&s, "削除できませんでした", &e.to_string());
        }
        s.wake();
    });
}

/// Web の「連携アプリ」管理ページ。
pub fn manage_url(shared: &Arc<Shared>) -> String {
    let s = shared.engine.settings().get();
    join_web(&s, &s.advanced.manage_apps_path)
}

/// 403（BAN など）の案内先のお問い合わせページ（Q-024・Q-027）。
pub fn contact_url(shared: &Arc<Shared>) -> String {
    let s = shared.engine.settings().get();
    join_web(&s, &s.advanced.contact_path)
}

/// ダウンロードページ（更新履歴）。
pub fn download_page_url(shared: &Arc<Shared>) -> String {
    let s = shared.engine.settings().get();
    join_web(&s, &s.advanced.download_page_path)
}

fn join_web(s: &Settings, path: &str) -> String {
    let base = s.advanced.api_base_url.trim_end_matches('/');
    format!("{base}{path}")
}

/// ログのフォルダを開く。
pub fn open_logs(shared: &Arc<Shared>) {
    let dir = shared.engine.paths().log_dir();
    if !crate::platform::open_path(&dir) {
        tracing::warn!("ログフォルダを開けませんでした");
    }
}

/// 診断情報（問い合わせ用）。個人情報・トークン・パスは含めない。
pub fn diagnostics(shared: &Arc<Shared>) -> String {
    let s = shared.engine.settings().get();
    let snap = shared.engine.snapshot().borrow().clone();
    let account = match snap.account {
        AccountView::Anonymous => "未ログイン",
        AccountView::LoggedIn { .. } => "ログイン中",
        AccountView::NeedsRelogin { .. } => "要再ログイン",
    };
    let host = url::Url::parse(&s.advanced.api_base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let watching = snap.folders.iter().filter(|f| f.watching).count();
    [
        format!("Clipshare Quick {}", cq_core::APP_VERSION),
        format!("Windows: {}", windows_version()),
        format!(
            "パッケージ: {}",
            if crate::platform::is_packaged() {
                "MSIX"
            } else {
                "なし"
            }
        ),
        format!("アカウント: {account}"),
        format!(
            "自動アップロード: {}",
            if s.auto_upload_active() { "ON" } else { "OFF" }
        ),
        format!(
            "撮影後にURLをコピー: {}",
            if s.copy_url_after_upload { "ON" } else { "OFF" }
        ),
        format!("既定の画質: {}", s.default_quality.label()),
        format!(
            "監視フォルダ: {} 件（監視中 {watching} 件）",
            s.watch_folders.len()
        ),
        format!(
            "動画の自動アップロード: {}",
            if s.watch_videos { "ON" } else { "OFF" }
        ),
        format!(
            "右クリックメニュー: {}",
            if s.context_menu_enabled { "ON" } else { "OFF" }
        ),
        format!("SteamVR: {}", if s.steamvr_enabled { "ON" } else { "OFF" }),
        format!("接続先: {host}"),
        format!("ログレベル: {}", s.advanced.log_level.as_filter()),
        format!("状態: {}", snap.tray.label()),
        format!("履歴: {} 件", snap.history.len()),
    ]
    .join("\n")
}

fn windows_version() -> String {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let mut buf = [0u16; 64];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: 出力バッファとサイズを正しく渡している。
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            w!("CurrentBuild"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return "不明".to_owned();
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    format!("build {}", String::from_utf16_lossy(&buf[..len]))
}

/// 右クリックメニューの表示可否をエクスプローラー拡張へ伝えるフラグファイル。
pub fn apply_context_menu_flag(shared: &Arc<Shared>, enabled: bool) {
    let flag = shared.engine.paths().shell_flag_file();
    let r = if enabled {
        match std::fs::remove_file(&flag) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::write(&flag, b"1")
    };
    if let Err(e) = r {
        tracing::warn!(error = %e, "右クリックメニューの設定を反映できませんでした");
    }
}

/// 設定の変更を OS 側（スタートアップ・右クリックメニュー・ログレベル・SteamVR）へ反映し続ける。
pub fn spawn_settings_effects(shared: &Arc<Shared>) {
    let s = Arc::clone(shared);
    let mut rx = shared.engine.settings().subscribe();
    shared.rt.spawn(async move {
        let mut prev: Option<Settings> = None;
        loop {
            let cur = rx.borrow_and_update().clone();
            let changed = |f: fn(&Settings) -> bool| prev.as_ref().is_none_or(|p| f(p) != f(&cur));
            if cur.onboarding_completed
                && (changed(|x| x.launch_at_startup) || changed(|x| x.onboarding_completed))
            {
                let on = cur.launch_at_startup;
                if let Err(e) =
                    tokio::task::spawn_blocking(move || crate::platform::set_launch_at_startup(on))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                {
                    tracing::warn!(error = %e, "スタートアップ登録を変更できませんでした");
                }
            }
            if changed(|x| x.context_menu_enabled) {
                apply_context_menu_flag(&s, cur.context_menu_enabled);
            }
            if prev
                .as_ref()
                .is_none_or(|p| p.advanced.log_level != cur.advanced.log_level)
                && let Some(log) = &s.log
            {
                log.set_level(cur.advanced.log_level);
            }
            if prev.is_some() && changed(|x| x.steamvr_enabled) {
                crate::vr_bridge::set_registered(cur.steamvr_enabled);
            }
            prev = Some(cur);
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Q-027: 既定の設定では 403 の案内先が `https://clipshare.link/contact` になる。
    #[test]
    fn default_contact_url_is_clipshare_contact_page() {
        let s = Settings::default();
        assert_eq!(
            join_web(&s, &s.advanced.contact_path),
            "https://clipshare.link/contact"
        );
    }

    #[test]
    fn join_web_ignores_trailing_slash_of_base() {
        let mut s = Settings::default();
        s.advanced.api_base_url = "https://clipshare.link/".to_owned();
        assert_eq!(
            join_web(&s, &s.advanced.contact_path),
            "https://clipshare.link/contact"
        );
        assert_eq!(
            join_web(&s, &s.advanced.manage_apps_path),
            "https://clipshare.link/settings/apps"
        );
        assert_eq!(
            join_web(&s, &s.advanced.download_page_path),
            "https://clipshare.link/qick/download"
        );
    }
}
