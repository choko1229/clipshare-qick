//! Clipshare Quick 本体（トレイ常駐アプリ）。
//!
//! 起動引数:
//! - なし: 常駐を開始して履歴パネルを開く（初回はセットアップ）。すでに動いていれば履歴パネルを開かせる。
//! - `--background`: 常駐だけ開始する（Windows 起動時・SteamVR から）。
//! - `--share <default|original|shrink> <ファイル>...`: ファイルを共有する（右クリック拡張から）。
//! - `--show <history|settings>`: 画面を開く。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use cq_core::engine::{Engine, EngineConfig};
use cq_core::ipc::{IpcServer, IpcServerError, Request, ShareQuality, View};
use cq_core::paths::AppPaths;
use cq_core::settings::{Quality, SettingsStore};
use cq_core::token_store::{CredentialManagerStore, target_name_for};

use clipshare_quick::app::{Command, Screen, SettingsTab, Shared};
use clipshare_quick::{actions, clipboard, host, platform, toast, tray, ui, updater, vr_bridge};

/// 起動引数。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    Normal,
    Background,
    Share {
        quality: ShareQuality,
        paths: Vec<PathBuf>,
    },
    Show(View),
}

fn parse_args(args: &[String]) -> Launch {
    match args.first().map(String::as_str) {
        Some("--background") => Launch::Background,
        Some("--share") => {
            let quality = match args.get(1).map(String::as_str) {
                Some("original") => ShareQuality::Original,
                Some("shrink") => ShareQuality::Shrink,
                _ => ShareQuality::Default,
            };
            let paths: Vec<PathBuf> = args
                .iter()
                .skip(2)
                .take(cq_ipc::MAX_SHARE_FILES)
                .map(PathBuf::from)
                .collect();
            if paths.is_empty() {
                Launch::Normal
            } else {
                Launch::Share { quality, paths }
            }
        }
        Some("--show") => match args.get(1).map(String::as_str) {
            Some("settings") => Launch::Show(View::Settings),
            _ => Launch::Show(View::History),
        },
        _ => Launch::Normal,
    }
}

fn to_request(l: &Launch) -> Option<Request> {
    match l {
        Launch::Normal => Some(Request::Show {
            view: View::History,
        }),
        Launch::Background => None,
        Launch::Share { quality, paths } => Some(Request::Share {
            quality: *quality,
            paths: paths.clone(),
        }),
        Launch::Show(v) => Some(Request::Show { view: *v }),
    }
}

fn quality_of(q: ShareQuality) -> Option<Quality> {
    match q {
        ShareQuality::Default => None,
        ShareQuality::Original => Some(Quality::Original),
        ShareQuality::Shrink => Some(Quality::Shrink),
    }
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        tracing::error!(location = %loc, "予期しないエラーが発生しました（panic）");
    }));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = parse_args(&args);

    let Some(root) = AppPaths::default_root() else {
        return;
    };
    let paths = AppPaths::new(root);
    let settings = SettingsStore::load(&paths.settings_file());
    let log = cq_core::logging::init(&paths.log_dir(), settings.get().advanced.log_level)
        .ok()
        .map(Arc::new);
    install_panic_hook();

    // すでに動いていれば、要求を渡して終わる（二重起動しない）。
    if let Some(req) = to_request(&launch) {
        match cq_ipc::send_request(&req, Duration::from_secs(2)) {
            Ok(_) => {
                tracing::info!("起動中のアプリに要求を渡しました");
                return;
            }
            Err(cq_ipc::IpcError::NotRunning) => {}
            Err(e) => {
                tracing::warn!(error = %e, "起動中のアプリに接続できませんでした");
                return;
            }
        }
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "実行環境を初期化できませんでした");
            return;
        }
    };
    let _guard = rt.enter();

    let (ipc_tx, mut ipc_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ipc = match IpcServer::start(ipc_tx) {
        Ok(s) => s,
        Err(IpcServerError::AlreadyRunning) => {
            if let Some(req) = to_request(&launch) {
                let _ = cq_ipc::send_request(&req, Duration::from_secs(2));
            }
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "IPC を開始できませんでした");
            return;
        }
    };

    let api_base = url::Url::parse(&settings.get().advanced.api_base_url).ok();
    let target = api_base
        .as_ref()
        .map(target_name_for)
        .unwrap_or_else(|| format!("{}default", cq_core::token_store::TARGET_PREFIX));
    let engine = match Engine::start(EngineConfig {
        paths: paths.clone(),
        settings: settings.clone(),
        token_store: Arc::new(CredentialManagerStore::new(target)),
        clipboard: Arc::new(clipboard::SystemClipboard),
    }) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "エンジンを開始できませんでした");
            return;
        }
    };

    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let shared = Arc::new(Shared::new(
        engine.clone(),
        rt.handle().clone(),
        ui::theme::system_palette(),
        cmd_tx,
        platform::current_thread_id(),
        log,
    ));

    let tray = match tray::Tray::new(&shared) {
        Ok(t) => Rc::new(RefCell::new(t)),
        Err(e) => {
            tracing::error!(error = %e, "トレイアイコンを作れませんでした");
            return;
        }
    };
    toast::spawn_event_loop(&shared);
    actions::spawn_settings_effects(&shared);
    updater::spawn_periodic(&shared);
    vr_bridge::start(&shared);

    // 他のプロセス（右クリック拡張・2 つ目の起動）からの要求。
    let s = Arc::clone(&shared);
    rt.spawn(async move {
        while let Some(req) = ipc_rx.recv().await {
            match req {
                Request::Share { quality, paths } => {
                    if s.engine.settings().get().onboarding_completed {
                        s.engine.share_files(&paths, quality_of(quality));
                    } else {
                        tracing::info!("初回設定の前のため、共有せずに初回設定を開きます");
                        s.send(Command::Open(Screen::Wizard));
                    }
                }
                Request::Show { view } => {
                    let screen = match view {
                        View::History => Screen::History,
                        View::Settings => Screen::Settings(SettingsTab::Upload),
                    };
                    s.send(Command::Open(screen));
                }
            }
        }
    });

    let onboarded = settings.get().onboarding_completed;
    let first = match &launch {
        _ if !onboarded => Some(Screen::Wizard),
        Launch::Background => None,
        Launch::Share { quality, paths } if onboarded => {
            engine.share_files(paths, quality_of(*quality));
            None
        }
        Launch::Share { .. } => Some(Screen::Wizard),
        Launch::Show(View::Settings) => Some(Screen::Settings(SettingsTab::Upload)),
        Launch::Normal | Launch::Show(View::History) => Some(Screen::History),
    };
    tracing::info!(?launch, "Clipshare Quick を開始しました");

    host::run(Arc::clone(&shared), cmd_rx, tray, first);

    tracing::info!("終了します");
    engine.shutdown();
    rt.shutdown_timeout(Duration::from_secs(3));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn argument_parsing() {
        assert_eq!(parse_args(&[]), Launch::Normal);
        assert_eq!(parse_args(&s(&["--background"])), Launch::Background);
        assert_eq!(
            parse_args(&s(&["--share", "shrink", r"C:\a.png", r"C:\b.png"])),
            Launch::Share {
                quality: ShareQuality::Shrink,
                paths: vec![PathBuf::from(r"C:\a.png"), PathBuf::from(r"C:\b.png")]
            }
        );
        assert_eq!(parse_args(&s(&["--share", "default"])), Launch::Normal);
        assert_eq!(
            parse_args(&s(&["--show", "settings"])),
            Launch::Show(View::Settings)
        );
        assert_eq!(parse_args(&s(&["--unknown"])), Launch::Normal);
        assert!(to_request(&Launch::Background).is_none());
    }
}
