//! SteamVR 連携の橋渡し（エンジンのイベント → HUD 通知、履歴 → ダッシュボードパネル、パネルの操作 → エンジン）。

use std::sync::{Arc, OnceLock};

use chrono::{Local, Utc};
use cq_core::engine::{AccountView, EngineEvent, Snapshot};
use cq_core::history::{DisplayStatus, EntrySource};
use cq_core::media::MediaKind;
use cq_vr::{
    HudMessage, HudTone, PanelAction, PanelItem, PanelModel, PanelStatus, VrCommand, VrConfig,
    VrService,
};

use crate::app::Shared;

static SERVICE: OnceLock<VrService> = OnceLock::new();

fn send(c: VrCommand) {
    if let Some(s) = SERVICE.get() {
        s.send(c);
    }
}

/// SteamVR から起動するときの実行ファイル。パッケージ版は実行エイリアス（`clipshare-quick.exe`）を使う。
fn launch_exe() -> std::path::PathBuf {
    if crate::platform::is_packaged()
        && let Some(local) = std::env::var_os("LOCALAPPDATA")
    {
        return std::path::PathBuf::from(local)
            .join("Microsoft")
            .join("WindowsApps")
            .join("clipshare-quick.exe");
    }
    std::env::current_exe().unwrap_or_default()
}

/// VR 連携を開始する（SteamVR が動いていなければ待つだけ）。
pub fn start(shared: &Arc<Shared>) {
    let settings = shared.engine.settings().get();
    let s = Arc::clone(shared);
    let svc = VrService::start(
        VrConfig {
            manifest_dir: shared.engine.paths().root().join("steamvr"),
            exe_path: launch_exe(),
            register: settings.steamvr_enabled && !crate::platform::system_changes_disabled(),
        },
        Box::new(move |a| on_panel_action(&s, a)),
    );
    if SERVICE.set(svc).is_err() {
        return;
    }
    // 履歴が変わったらパネルを更新する。
    let mut rx = shared.engine.snapshot();
    shared.rt.spawn(async move {
        let mut last: Option<PanelModel> = None;
        loop {
            let model = panel_model(&rx.borrow_and_update());
            if last.as_ref() != Some(&model) {
                send(VrCommand::Panel(model.clone()));
                last = Some(model);
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
}

pub fn set_registered(on: bool) {
    send(VrCommand::SetRegistered(
        on && !crate::platform::system_changes_disabled(),
    ));
}

/// パネルの内容を作る（直近 6 件、期限切れ・削除済みは除く）。
pub fn panel_model(snap: &Snapshot) -> PanelModel {
    let now = Utc::now();
    let user = match &snap.account {
        AccountView::LoggedIn { name } | AccountView::NeedsRelogin { name } => name.clone(),
        AccountView::Anonymous => "未ログイン".to_owned(),
    };
    let items = snap
        .history
        .iter()
        .filter_map(|e| {
            let status = match e.display_status(now) {
                DisplayStatus::Done => PanelStatus::Ready {
                    remaining: e.remaining_label(now).unwrap_or_default(),
                },
                DisplayStatus::Processing => PanelStatus::Processing,
                DisplayStatus::Failed => PanelStatus::Failed,
                _ => return None,
            };
            Some(PanelItem {
                id: e.id.clone(),
                time: e
                    .created_at
                    .with_timezone(&Local)
                    .format("%H:%M")
                    .to_string(),
                quality: if e.kind == MediaKind::Video {
                    "動画".to_owned()
                } else if e.shrunk {
                    "縮小".to_owned()
                } else {
                    e.quality.label().to_owned()
                },
                is_video: e.kind == MediaKind::Video,
                status,
                image: (e.kind == MediaKind::Image).then(|| e.path.clone()),
            })
        })
        .take(6)
        .collect();
    PanelModel {
        user,
        auto_upload: snap.auto_upload,
        items,
        confirm_delete: None,
    }
}

fn on_panel_action(shared: &Arc<Shared>, a: PanelAction) {
    match a {
        PanelAction::ToggleAutoUpload => {
            let on = !shared.engine.settings().get().auto_upload_active();
            if let Err(e) = shared.engine.set_auto_upload(on) {
                tracing::warn!(error = %e, "VR から自動アップロードを切り替えられませんでした");
            }
        }
        PanelAction::ConfirmDelete(id) => crate::actions::delete_entry(shared, id),
        PanelAction::Retry(id) => {
            if let Err(e) = shared.engine.retry(&id) {
                tracing::warn!(error = %e, "VR から再試行できませんでした");
            }
        }
        PanelAction::AskDelete(_) | PanelAction::CancelDelete => {}
    }
}

/// HUD に出す内容（画面 18）。
pub fn hud_for(ev: &EngineEvent) -> Option<HudMessage> {
    let m = |title: &str, subtitle: &str, tone| HudMessage {
        title: title.to_owned(),
        subtitle: subtitle.to_owned(),
        tone,
    };
    Some(match ev {
        EngineEvent::UploadStarted {
            source: EntrySource::Auto,
            ..
        } => m("アップロード中…", "写真を送っています", HudTone::Progress),
        EngineEvent::Uploaded {
            source: EntrySource::Auto,
            copied,
            copied_count,
            ..
        } => {
            if *copied {
                m(
                    "URLをコピーしました",
                    &format!("24時間有効 · {}件", (*copied_count).max(1)),
                    HudTone::Success,
                )
            } else {
                m("アップロードしました", "24時間有効", HudTone::Success)
            }
        }
        EngineEvent::VideoTooLarge { .. } => m(
            "アップロードできませんでした",
            "動画が上限を超えています",
            HudTone::Error,
        ),
        EngineEvent::Failed { code, message, .. }
            if code != "CANCELLED" && code != "VIDEO_TOO_LARGE" =>
        {
            m("アップロードできませんでした", message, HudTone::Error)
        }
        EngineEvent::AutoUploadChanged { enabled: false } => m(
            "自動アップロード OFF",
            "撮影してもアップロードしません",
            HudTone::Info,
        ),
        EngineEvent::AutoUploadChanged { enabled: true } => {
            m("自動アップロード ON", "", HudTone::Info)
        }
        _ => return None,
    })
}

pub fn on_event(shared: &Arc<Shared>, ev: &EngineEvent) {
    if !shared.engine.settings().get().notifications.vr_hud {
        return;
    }
    if let Some(h) = hud_for(ev) {
        send(VrCommand::Hud(h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hud_texts_follow_design() {
        let h = hud_for(&EngineEvent::AutoUploadChanged { enabled: false }).unwrap();
        assert_eq!(h.title, "自動アップロード OFF");
        assert_eq!(h.subtitle, "撮影してもアップロードしません");
        let h = hud_for(&EngineEvent::VideoTooLarge {
            file_name: "x.mp4".into(),
            size_bytes: 1,
        })
        .unwrap();
        assert_eq!(h.subtitle, "動画が上限を超えています");
        assert!(hud_for(&EngineEvent::LoggedOut).is_none());
    }
}
