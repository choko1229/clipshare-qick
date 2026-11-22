//! 右クリック共有の進捗（画面 15）。

use std::sync::Arc;

use cq_core::engine::{BatchItemStatus, BatchView};
use egui::{Align, Layout, RichText, Ui};

use super::theme::{Palette, bold, regular};
use super::widgets::{self, Tone};
use crate::app::{Screen, Shared};
use crate::host::Action;

#[derive(Debug)]
pub struct ProgressWindow {
    batch_id: String,
}

impl ProgressWindow {
    pub fn new(batch_id: String) -> Self {
        Self { batch_id }
    }

    pub fn ui(&mut self, ui: &mut Ui, shared: &Arc<Shared>) -> Action {
        let p = shared.palette;
        let Some(view) = shared.engine.batch(&self.batch_id) else {
            return Action::Close;
        };
        let mut action = Action::None;
        egui::Frame::new()
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let total = view.items.len();
                let done = view
                    .items
                    .iter()
                    .filter(|i| i.status == BatchItemStatus::Done)
                    .count();
                let failed = view
                    .items
                    .iter()
                    .filter(|i| matches!(i.status, BatchItemStatus::Failed(_)))
                    .count();
                if view.finished && failed == 0 {
                    action = finished_ok(ui, &p, &view);
                } else {
                    let title = if view.finished {
                        format!("{done}件完了・{failed}件失敗")
                    } else {
                        format!("共有中 ({}/{total})", (done + failed + 1).min(total))
                    };
                    ui.label(widgets::heading(&title, &p));
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(ui.available_height() - 56.0)
                        .show(ui, |ui| {
                            for it in &view.items {
                                item_row(ui, &p, &it.file_name, &it.status, it.progress);
                            }
                        });
                    widgets::bottom_bar(ui, &p, |ui| {
                        if view.finished {
                            if widgets::secondary(ui, &p, "閉じる").clicked() {
                                action = Action::Close;
                            }
                        } else if widgets::secondary(ui, &p, "キャンセル").clicked() {
                            shared.engine.cancel_batch(&self.batch_id);
                        }
                    });
                }
            });
        action
    }
}

fn finished_ok(ui: &mut Ui, p: &Palette, view: &BatchView) -> Action {
    let mut action = Action::None;
    ui.label(widgets::heading(
        &format!("{}件のURLをコピーしました", view.urls.len()),
        p,
    ));
    ui.label(widgets::muted(
        "改行区切りでクリップボードに入れました。そのまま貼り付けられます。",
        p,
    ));
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(p.mut_)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(110.0)
                .show(ui, |ui| {
                    for u in &view.urls {
                        ui.label(RichText::new(u).font(regular(12.0)).color(p.fg));
                    }
                });
        });
    widgets::bottom_bar(ui, p, |ui| {
        if widgets::primary(ui, p, "閉じる").clicked() {
            action = Action::Close;
        }
        if widgets::secondary(ui, p, "履歴を開く").clicked() {
            action = Action::Open(Screen::History);
        }
    });
    action
}

fn item_row(ui: &mut Ui, p: &Palette, name: &str, status: &BatchItemStatus, progress: f32) {
    ui.horizontal(|ui| {
        ui.set_min_height(36.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width() - 90.0);
            ui.add(egui::Label::new(RichText::new(name).font(bold(13.0)).color(p.fg)).truncate());
            match status {
                BatchItemStatus::Uploading => {
                    widgets::progress_bar(ui, p, progress, ui.available_width() - 8.0)
                }
                BatchItemStatus::Failed(reason) => {
                    ui.add(
                        egui::Label::new(RichText::new(reason).font(regular(11.5)).color(p.det))
                            .truncate(),
                    );
                }
                _ => {}
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match status {
            BatchItemStatus::Done => widgets::badge(ui, p, "完了", Tone::Primary),
            BatchItemStatus::Uploading => {
                ui.label(
                    RichText::new(format!("{}%", (progress * 100.0).round() as u32))
                        .font(bold(12.0))
                        .color(p.mfg),
                );
            }
            BatchItemStatus::Waiting => widgets::badge(ui, p, "待機中", Tone::Muted),
            BatchItemStatus::Failed(_) => widgets::badge(ui, p, "失敗", Tone::Danger),
        });
    });
}
