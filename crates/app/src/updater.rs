//! 更新（画面 21）: 確認 → 通知 → 利用者が選ぶ → ダウンロード → ハッシュ・署名の検証 → 適用。
//!
//! - 確認は起動時と、設定の間隔（既定 24 時間）ごと。`GET /api/v1/client/latest` を使う。
//! - 勝手に適用・再起動しない（VRChat のプレイ中でも安全）。
//! - ハッシュ・サイズ・署名のどれかが一致しなければ適用しない。

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use cq_core::api::ReleaseAsset;
use cq_core::update::{UpdateDecision, decide, should_check, verify_download};

use crate::app::{Shared, UpdateFailure, UpdateState};
use crate::packaging::{InstallOutcome, install_update, register_restart, verify_signature};

/// 確認で見つかった更新（ダウンロード用に保持）。
static FOUND: std::sync::Mutex<Option<(String, ReleaseAsset)>> = std::sync::Mutex::new(None);

fn set_found(v: Option<(String, ReleaseAsset)>) {
    if let Ok(mut g) = FOUND.lock() {
        *g = v;
    }
}

fn found() -> Option<(String, ReleaseAsset)> {
    FOUND.lock().ok().and_then(|g| g.clone())
}

/// 起動時と定期的に確認する。
pub fn spawn_periodic(shared: &Arc<Shared>) {
    let s = Arc::clone(shared);
    shared.rt.spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            let settings = s.engine.settings().get();
            if settings.auto_update_check && settings.onboarding_completed {
                check(&s, false).await;
            }
            tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        }
    });
}

/// すぐに確認する（設定の「更新を確認」など）。
pub fn check_now(shared: &Arc<Shared>, user_initiated: bool) {
    let s = Arc::clone(shared);
    shared.set_update_state(UpdateState::Checking);
    shared
        .rt
        .spawn(async move { check(&s, user_initiated).await });
}

async fn check(shared: &Arc<Shared>, user_initiated: bool) {
    let interval = shared
        .engine
        .settings()
        .get()
        .advanced
        .update_check_interval_hours;
    let last = shared.engine.last_update_check();
    if !user_initiated && !should_check(last, Utc::now(), interval) {
        return;
    }
    shared.engine.set_last_update_check(Utc::now());
    let latest = match shared.engine.api().latest_client().await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(code = e.code(), "更新情報を取得できませんでした");
            if user_initiated {
                shared.set_update_state(UpdateState::Failed {
                    reason: UpdateFailure::Network,
                });
            }
            return;
        }
    };
    match decide(cq_core::APP_VERSION, &latest) {
        Ok(UpdateDecision::Available {
            version,
            asset,
            notes,
            ..
        }) => {
            let v = format!("v{version}");
            tracing::info!(version = %v, "新しいバージョンがあります");
            set_found(Some((v.clone(), asset.clone())));
            shared.set_update_state(UpdateState::Available {
                version: v.clone(),
                size_bytes: asset.size_bytes,
                notes,
            });
            if !user_initiated {
                crate::toast::show(
                    shared,
                    &crate::toast::ToastSpec {
                        title: "新しいバージョンがあります".into(),
                        body: format!("v{} → {v}", cq_core::APP_VERSION),
                        buttons: vec![crate::toast::ToastButton {
                            label: "ダウンロードして更新",
                            action: crate::toast::ToastAction::Update,
                        }],
                        tag: Some("update"),
                        launch: crate::toast::ToastAction::Update,
                    },
                );
            }
        }
        Ok(UpdateDecision::ManualOnly { version, notes }) => {
            let v = format!("v{version}");
            tracing::info!(version = %v, "新しいバージョンがあります（検証用のハッシュが無いためダウンロードページへ案内）");
            set_found(None);
            shared.set_update_state(UpdateState::ManualAvailable {
                version: v.clone(),
                notes,
            });
            if !user_initiated {
                crate::toast::show(
                    shared,
                    &crate::toast::ToastSpec {
                        title: "新しいバージョンがあります".into(),
                        body: format!("v{} → {v}", cq_core::APP_VERSION),
                        buttons: vec![],
                        tag: Some("update"),
                        launch: crate::toast::ToastAction::Update,
                    },
                );
            }
        }
        Ok(UpdateDecision::UpToDate | UpdateDecision::NoAsset) => {
            if user_initiated {
                shared.set_update_state(UpdateState::UpToDate);
            }
        }
        Err(e) => tracing::warn!(error = %e, "更新情報の形式が不正です"),
    }
}

/// ダウンロードして検証する。
pub fn download(shared: &Arc<Shared>) {
    let Some((version, asset)) = found() else {
        return;
    };
    let s = Arc::clone(shared);
    let cancel = tokio_util::sync::CancellationToken::new();
    shared.set_update_cancel(Some(cancel.clone()));
    shared.set_update_state(UpdateState::Downloading {
        version: version.clone(),
        fraction: 0.0,
        bytes_per_sec: 0.0,
    });
    shared.rt.spawn(async move {
        let dir = s.engine.paths().download_dir();
        let safe_name: String = asset
            .name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
            .collect();
        let dest = dir.join(if safe_name.is_empty() {
            "update.msix".to_owned()
        } else {
            safe_name
        });
        let started = Instant::now();
        let s2 = Arc::clone(&s);
        let v2 = version.clone();
        let progress: cq_core::api::ProgressFn = Arc::new(move |done, total| {
            let secs = started.elapsed().as_secs_f64().max(0.001);
            s2.set_update_state(UpdateState::Downloading {
                version: v2.clone(),
                fraction: if total == 0 {
                    0.0
                } else {
                    done as f32 / total as f32
                },
                bytes_per_sec: done as f64 / secs,
            });
        });
        let max = asset.size_bytes.saturating_add(1024);
        let result = s
            .engine
            .api()
            .download_to(&asset.url, &dest, max, Some(progress), &cancel)
            .await;
        s.set_update_cancel(None);
        let sha = match result {
            Ok(h) => h,
            Err(cq_core::api::ApiError::Cancelled) => {
                s.set_update_state(UpdateState::Available {
                    version,
                    size_bytes: asset.size_bytes,
                    notes: String::new(),
                });
                return;
            }
            Err(e) => {
                tracing::warn!(
                    code = e.code(),
                    "更新ファイルをダウンロードできませんでした"
                );
                s.set_update_state(UpdateState::Failed {
                    reason: UpdateFailure::Network,
                });
                return;
            }
        };
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        let verified = verify_download(&asset, &sha, size).is_ok() && {
            let d = dest.clone();
            tokio::task::spawn_blocking(move || verify_signature(&d).is_ok())
                .await
                .unwrap_or(false)
        };
        if !verified {
            tracing::warn!("更新ファイルの検証に失敗したため適用しません");
            let _ = std::fs::remove_file(&dest);
            s.set_update_state(UpdateState::Failed {
                reason: UpdateFailure::Signature,
            });
            return;
        }
        s.set_update_state(UpdateState::Ready {
            version,
            file: dest,
        });
    });
}

fn ready_file(shared: &Arc<Shared>) -> Option<std::path::PathBuf> {
    match shared.update_state() {
        UpdateState::Ready { file, .. } => Some(file),
        _ => None,
    }
}

/// 今すぐ適用する（アップロード中なら終わるまで待つ）。
pub fn apply_now(shared: &Arc<Shared>) {
    let Some(file) = ready_file(shared) else {
        return;
    };
    let s = Arc::clone(shared);
    shared.rt.spawn(async move {
        // アップロード中のファイルがあれば完了を待つ（最大 10 分）。
        for _ in 0..600 {
            let busy = s.engine.snapshot().borrow().tray == cq_core::engine::TrayState::Uploading;
            if !busy {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        register_restart();
        let f = file.clone();
        let r = tokio::task::spawn_blocking(move || install_update(&f, true)).await;
        match r {
            Ok(Ok(InstallOutcome::Applied)) => tracing::info!("更新を適用しました"),
            Ok(Ok(InstallOutcome::Deferred)) => {
                crate::toast::show_simple(
                    &s,
                    "更新の準備ができました",
                    "PCを再起動すると更新されます。",
                );
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "更新を適用できませんでした");
                s.set_update_state(UpdateState::Failed {
                    reason: UpdateFailure::Install,
                });
            }
            Err(e) => tracing::error!(error = %e, "更新の処理が中断しました"),
        }
    });
}

/// 次回起動時に適用されるよう予約する。
pub fn apply_on_next_start(shared: &Arc<Shared>) {
    let Some(file) = ready_file(shared) else {
        return;
    };
    let s = Arc::clone(shared);
    shared.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || install_update(&file, false)).await;
        match r {
            Ok(Ok(_)) => tracing::info!("次回起動時に更新されるよう予約しました"),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "更新を予約できませんでした");
                s.set_update_state(UpdateState::Failed {
                    reason: UpdateFailure::Install,
                });
            }
            Err(e) => tracing::error!(error = %e, "更新の処理が中断しました"),
        }
    });
}
