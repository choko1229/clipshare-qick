//! アンインストール（画面 22）。
//!
//! MSIX にはアンインストール時に処理を差し込む仕組みが無いため、アプリ内から後片付け
//! （アップロード済みファイルの削除・連携解除・ローカルデータの削除・SteamVR 登録の解除）を行ってから、
//! パッケージの削除を Windows に依頼する。

use std::sync::{Arc, Mutex};

use cq_core::engine::{UninstallOptions, UninstallReport, UninstallStep};
use egui::{RichText, Ui};

use super::theme::{Palette, regular};
use super::widgets::{self, Tone};
use crate::app::Shared;
use crate::host::Action;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Confirm,
    Running {
        step: usize,
    },
    Done {
        report: UninstallReport,
        removed: bool,
    },
}

#[derive(Debug)]
pub struct UninstallWindow {
    delete_local: bool,
    unlink: bool,
    delete_uploads: bool,
    phase: Arc<Mutex<Phase>>,
}

impl Default for UninstallWindow {
    fn default() -> Self {
        Self {
            delete_local: true,
            unlink: true,
            delete_uploads: false,
            phase: Arc::new(Mutex::new(Phase::Confirm)),
        }
    }
}

const STEPS: [&str; 4] = [
    "アプリを終了",
    "右クリックメニュー拡張の登録を解除",
    "Clipshareの連携トークンを無効化",
    "ファイルを削除",
];

fn set(phase: &Arc<Mutex<Phase>>, p: Phase) {
    if let Ok(mut g) = phase.lock() {
        *g = p;
    }
}

impl UninstallWindow {
    pub fn ui(&mut self, ui: &mut Ui, shared: &Arc<Shared>) -> Action {
        let p = shared.palette;
        let phase = self
            .phase
            .lock()
            .map(|g| g.clone())
            .unwrap_or(Phase::Confirm);
        let mut action = Action::None;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(28, 24))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                action = match phase {
                    Phase::Confirm => self.confirm(ui, &p, shared),
                    Phase::Running { step } => running(ui, &p, step),
                    Phase::Done { report, removed } => done(ui, &p, shared, report, removed),
                };
            });
        action
    }

    fn confirm(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        let mut action = Action::None;
        ui.label(widgets::title("アンインストールしますか？", p));
        ui.label(widgets::body(
            "Clipshare Quick を、このPCから削除します。アップロード済みのURLはそのまま使えます（24時間で自動的に消えます）。",
            p,
        ));
        ui.add_space(6.0);
        ui.label(widgets::strong("あわせて削除する項目", p));
        for (value, title, desc) in [
            (
                &mut self.delete_local,
                "このPCの設定と履歴",
                "ローカルに保存した設定・履歴・ログを削除します。",
            ),
            (
                &mut self.unlink,
                "Clipshareとの連携を解除する",
                "このPCの連携トークンを無効にします。オフラインの場合はWebの「連携アプリ」から解除できます。",
            ),
            (
                &mut self.delete_uploads,
                "アップロード済みの写真・動画を今すぐ削除する",
                "履歴にあるURLを、サーバーからすぐ削除します。",
            ),
        ] {
            ui.horizontal(|ui| {
                ui.checkbox(value, "");
                ui.vertical(|ui| {
                    ui.label(widgets::strong(title, p));
                    ui.label(widgets::muted(desc, p));
                });
            });
        }
        widgets::bottom_bar(ui, p, |ui| {
            if widgets::danger(ui, p, "アンインストール").clicked() {
                self.start(shared);
            }
            if widgets::secondary(ui, p, "キャンセル").clicked() {
                action = Action::Close;
            }
        });
        action
    }

    fn start(&self, shared: &Arc<Shared>) {
        set(&self.phase, Phase::Running { step: 0 });
        let opts = UninstallOptions {
            delete_local_data: self.delete_local,
            unlink: self.unlink,
            delete_uploads: self.delete_uploads,
        };
        let phase = Arc::clone(&self.phase);
        let s = Arc::clone(shared);
        shared.rt.spawn(async move {
            tracing::info!(?opts, "アンインストールを開始します");
            set(&phase, Phase::Running { step: 1 });
            // 拡張の表示を止め、SteamVR の登録を外す。
            crate::actions::apply_context_menu_flag(&s, false);
            crate::vr_bridge::set_registered(false);
            if let Err(e) =
                tokio::task::spawn_blocking(|| crate::platform::set_launch_at_startup(false)).await
            {
                tracing::warn!(error = %e, "スタートアップ登録を外せませんでした");
            }
            crate::toast::unregister();
            let p2 = Arc::clone(&phase);
            let report = s
                .engine
                .cleanup_for_uninstall(opts, move |st| {
                    let step = match st {
                        UninstallStep::DeletingUploads | UninstallStep::RevokingToken => 2,
                        UninstallStep::DeletingFiles => 3,
                    };
                    set(&p2, Phase::Running { step });
                })
                .await;
            if opts.delete_local_data {
                let _ = std::fs::remove_dir_all(s.engine.paths().log_dir());
            }
            set(&phase, Phase::Running { step: 4 });
            set(
                &phase,
                Phase::Done {
                    report,
                    removed: crate::platform::is_packaged(),
                },
            );
            s.wake();
        });
    }
}

fn running(ui: &mut Ui, p: &Palette, step: usize) -> Action {
    ui.label(widgets::title("アンインストールしています…", p));
    ui.add_space(4.0);
    widgets::progress_bar(
        ui,
        p,
        step as f32 / STEPS.len() as f32,
        ui.available_width(),
    );
    for (i, s) in STEPS.iter().enumerate() {
        let mark = if i < step {
            "✓"
        } else if i == step {
            "…"
        } else {
            "・"
        };
        let color = if i <= step { p.fg } else { p.mfg };
        ui.label(
            RichText::new(format!("{mark} {s}"))
                .font(regular(13.0))
                .color(color),
        );
    }
    Action::None
}

fn done(
    ui: &mut Ui,
    p: &Palette,
    shared: &Arc<Shared>,
    report: UninstallReport,
    removed: bool,
) -> Action {
    let mut action = Action::None;
    ui.label(widgets::title("アンインストールが完了しました", p));
    if let Some((title, body)) = unlink_notice(&report) {
        widgets::tinted(ui, p.was, p.was, |ui| {
            ui.label(widgets::strong(title, p));
            ui.label(widgets::muted(body, p));
        });
    }
    if report.uploads_not_deleted > 0 {
        widgets::badge(
            ui,
            p,
            &format!("{}件を削除できませんでした", report.uploads_not_deleted),
            Tone::Warning,
        );
    }
    ui.label(widgets::muted(
        "エクスプローラーの右クリックメニューは、再起動後に消えます。",
        p,
    ));
    widgets::bottom_bar(ui, p, |ui| {
        if widgets::primary(ui, p, "完了").clicked() {
            // パッケージの削除は、このプロセスの終了後に行われるよう Windows に依頼する。
            if removed && !crate::packaging::schedule_self_removal() {
                tracing::warn!("パッケージの削除を依頼できませんでした");
            }
            action = Action::Quit;
        }
        if !report.server_revoked && widgets::secondary(ui, p, "連携アプリを開く").clicked()
        {
            crate::platform::open_url(&crate::actions::manage_url(shared));
        }
    });
    action
}

/// 完了画面（画面 22）の「連携の解除が完了していません」の帯。サーバー側で解除できたときは出さない。
fn unlink_notice(report: &UninstallReport) -> Option<(&'static str, &'static str)> {
    (!report.server_revoked).then_some((
        crate::actions::UNLINK_INCOMPLETE_TITLE,
        crate::actions::UNLINK_INCOMPLETE_BODY,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 画面 22 の帯は通知（画面 16）と同じ文言（Q-033）。
    #[test]
    fn unlink_notice_uses_confirmed_text() {
        let mut r = UninstallReport {
            server_revoked: false,
            ..UninstallReport::default()
        };
        let (title, body) = unlink_notice(&r).unwrap();
        assert_eq!(title, "連携の解除が完了していません");
        assert_eq!(
            body,
            "連携の解除を完了できませんでした。連携アプリで状態を確認してください。"
        );
        let t = crate::toast::unlink_incomplete_spec();
        assert_eq!((t.title.as_str(), t.body.as_str()), (title, body));
        r.server_revoked = true;
        assert!(unlink_notice(&r).is_none());
    }
}
