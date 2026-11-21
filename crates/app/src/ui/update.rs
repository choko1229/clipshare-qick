//! アップデート（画面 21）。

use std::sync::Arc;

use egui::{RichText, Ui};

use super::theme::{Palette, regular};
use super::widgets::{self, Tone};
use crate::app::{Shared, UpdateFailure, UpdateState};
use crate::host::Action;

#[derive(Debug)]
pub struct UpdateWindow;

impl UpdateWindow {
    pub fn ui(&mut self, ui: &mut Ui, shared: &Arc<Shared>) -> Action {
        let p = shared.palette;
        let mut action = Action::None;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(28, 24)).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            action = match shared.update_state() {
                UpdateState::Idle | UpdateState::Checking => {
                    widgets::spinner(ui, &p, 24.0);
                    Action::None
                }
                UpdateState::UpToDate => {
                    ui.label(widgets::title(&format!("v{}", cq_core::APP_VERSION), &p));
                    widgets::badge(ui, &p, "最新です", Tone::Primary);
                    buttons(ui, &p, |ui| widgets::secondary(ui, &p, "閉じる").clicked().then_some(Action::Close))
                }
                UpdateState::Available { version, size_bytes, notes } => available(ui, &p, shared, &version, size_bytes, &notes),
                UpdateState::ManualAvailable { version, notes } => {
                    ui.label(widgets::title("新しいバージョンがあります", &p));
                    ui.label(widgets::muted(&format!("v{} → {version}", cq_core::APP_VERSION), &p));
                    ui.add_space(6.0);
                    widgets::card(ui, &p, |ui| {
                        ui.label(widgets::strong("変更点", &p));
                        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            for line in notes.lines().filter(|l| !l.trim().is_empty()).take(30) {
                                ui.label(RichText::new(line.trim()).font(regular(13.0)).color(p.fg));
                            }
                        });
                    });
                    buttons(ui, &p, |ui| {
                        if widgets::primary(ui, &p, "更新履歴").clicked() {
                            crate::platform::open_url(&crate::actions::download_page_url(shared));
                        }
                        widgets::secondary(ui, &p, "後で").clicked().then_some(Action::Close)
                    })
                }
                UpdateState::Downloading { version, fraction, bytes_per_sec } => {
                    ui.label(widgets::title("ダウンロードしています…", &p));
                    ui.label(widgets::strong(&version, &p));
                    widgets::progress_bar(ui, &p, fraction, ui.available_width());
                    ui.label(widgets::muted(
                        &format!(
                            "{}% · {}/s",
                            (fraction * 100.0).round() as u32,
                            cq_core::update::format_size(bytes_per_sec.max(0.0) as u64)
                        ),
                        &p,
                    ));
                    ui.label(widgets::muted("ダウンロード後に署名を確認してから適用します。", &p));
                    buttons(ui, &p, |ui| {
                        if widgets::secondary(ui, &p, "キャンセル").clicked() {
                            shared.cancel_update_download();
                        }
                        None
                    })
                }
                UpdateState::Ready { version, .. } => {
                    ui.label(widgets::title("更新の準備ができました", &p));
                    widgets::badge(ui, &p, "署名を確認しました", Tone::Primary);
                    ui.label(widgets::body(
                        &format!("再起動すると {version} に更新されます。アップロード中のファイルがある場合は、完了を待ってから再起動します。"),
                        &p,
                    ));
                    ui.label(widgets::muted("VRChatの撮影中でも、写真は更新後に自動で拾われます。", &p));
                    buttons(ui, &p, |ui| {
                        if widgets::primary(ui, &p, "今すぐ再起動して更新").clicked() {
                            crate::updater::apply_now(shared);
                            return Some(Action::None);
                        }
                        if widgets::secondary(ui, &p, "次回起動時に更新").clicked() {
                            crate::updater::apply_on_next_start(shared);
                            return Some(Action::Close);
                        }
                        None
                    })
                }
                UpdateState::Failed { reason } => failed(ui, &p, shared, reason),
            };
        });
        action
    }
}

fn available(
    ui: &mut Ui,
    p: &Palette,
    shared: &Arc<Shared>,
    version: &str,
    size: u64,
    notes: &str,
) -> Action {
    ui.label(widgets::title("新しいバージョンがあります", p));
    ui.label(widgets::muted(
        &format!(
            "v{} → {version}（{}）",
            cq_core::APP_VERSION,
            cq_core::update::format_size(size)
        ),
        p,
    ));
    ui.add_space(6.0);
    widgets::card(ui, p, |ui| {
        ui.label(widgets::strong("変更点", p));
        egui::ScrollArea::vertical()
            .max_height(100.0)
            .show(ui, |ui| {
                for line in notes.lines().filter(|l| !l.trim().is_empty()).take(30) {
                    ui.label(RichText::new(line.trim()).font(regular(13.0)).color(p.fg));
                }
            });
    });
    let mut auto = shared.engine.settings().get().auto_update_check;
    ui.horizontal(|ui| {
        if widgets::toggle(ui, p, &mut auto, "自動で更新を確認する").changed() {
            let _ = shared
                .engine
                .settings()
                .update(|s| s.auto_update_check = auto);
        }
        ui.vertical(|ui| {
            ui.label(widgets::strong("自動で更新を確認する", p));
            ui.label(widgets::muted("起動時と24時間ごとに確認します。", p));
        });
    });
    buttons(ui, p, |ui| {
        if widgets::primary(ui, p, "ダウンロードして更新").clicked() {
            crate::updater::download(shared);
        }
        if widgets::secondary(ui, p, "更新履歴").clicked() {
            crate::platform::open_url(&crate::actions::download_page_url(shared));
        }
        if widgets::secondary(ui, p, "後で").clicked() {
            return Some(Action::Close);
        }
        None
    })
}

fn failed(ui: &mut Ui, p: &Palette, shared: &Arc<Shared>, reason: UpdateFailure) -> Action {
    ui.label(widgets::title("更新できませんでした", p));
    let badge = match reason {
        UpdateFailure::Signature => "署名の確認に失敗しました",
        UpdateFailure::Network => "ダウンロードに失敗しました",
        UpdateFailure::Install => "適用に失敗しました",
    };
    widgets::badge(ui, p, badge, Tone::Danger);
    if reason == UpdateFailure::Signature {
        ui.label(widgets::body(
            "ダウンロードしたファイルが想定と一致しません。安全のため適用していません。",
            p,
        ));
    }
    ui.label(widgets::body("今のバージョンはそのまま使えます。", p));
    ui.label(widgets::muted(
        "ネットワークや保護ソフトの影響で起きることがあります。時間をおいて再試行してください。",
        p,
    ));
    buttons(ui, p, |ui| {
        if widgets::primary(ui, p, "再試行").clicked() {
            crate::updater::check_now(shared, true);
        }
        if widgets::secondary(ui, p, "ログを開く").clicked() {
            crate::actions::open_logs(shared);
        }
        None
    })
}

fn buttons(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> Option<Action>) -> Action {
    widgets::bottom_bar(ui, p, add).unwrap_or(Action::None)
}
