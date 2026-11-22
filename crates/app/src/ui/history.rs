//! トレイの履歴パネル（画面 08・09）。

use std::sync::Arc;

use chrono::Utc;
use cq_core::engine::{AccountView, Snapshot, TrayState};
use cq_core::history::{DisplayStatus, EntrySource, HistoryEntry};
use cq_core::media::MediaKind;
use cq_core::settings::Quality;
use egui::{Align, Layout, RichText, ScrollArea, Ui};

use super::theme::{Palette, bold, regular};
use super::widgets::{self, Tone};
use crate::app::{Command, Screen, SettingsTab, Shared};
use crate::host::Action;

#[derive(Debug)]
pub struct HistoryPanel {
    quality: Quality,
}

impl HistoryPanel {
    pub fn new(shared: &Arc<Shared>) -> Self {
        Self {
            quality: shared.engine.settings().get().default_quality,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, shared: &Arc<Shared>) -> Action {
        let p = shared.palette;
        let snap = shared.engine.snapshot().borrow().clone();
        let mut action = Action::None;
        egui::Frame::new()
            .fill(p.bg)
            .stroke(egui::Stroke::new(1.0_f32, p.bd))
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                action = self.header(ui, &p, &snap);
                ui.add_space(4.0);
                if !snap.history.is_empty() || !matches!(snap.account, AccountView::LoggedIn { .. }) || snap.offline {
                    self.controls(ui, &p, shared, &snap);
                }
                if let AccountView::Anonymous = snap.account
                    && !snap.history.is_empty()
                        && let Some(a) = anonymous_notice(ui, &p, &snap) {
                            action = a;
                        }
                if snap.blocked {
                    widgets::tinted(ui, p.des, p.des, |ui| {
                        ui.label(widgets::strong("このアカウントでは利用できません", &p));
                        if widgets::small(ui, &p, "お問い合わせ").clicked() {
                            shared.send(Command::OpenContactPage);
                        }
                    });
                }
                if snap.offline {
                    widgets::tinted(ui, p.was, p.was, |ui| {
                        ui.label(widgets::strong("オフラインです", &p));
                        ui.label(widgets::muted(
                            &format!("接続が戻ると、待機中の{}件を自動でアップロードします。", snap.waiting_count),
                            &p,
                        ));
                    });
                }
                let footer_h = 56.0;
                let list_h = (ui.available_height() - footer_h).max(80.0);
                if snap.history.is_empty() {
                    ui.allocate_ui(egui::vec2(ui.available_width(), list_h), |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(list_h * 0.25);
                            ui.label(widgets::heading("まだ写真がありません", &p));
                            ui.label(widgets::muted(
                                "VRChatで写真を撮ると、ここに表示されます。右クリックで共有したファイルも並びます。",
                                &p,
                            ));
                        });
                    });
                } else {
                    ui.label(RichText::new("直近のアップロード").font(bold(12.0)).color(p.mfg));
                    ScrollArea::vertical().max_height(list_h - 24.0).auto_shrink([false, true]).show(ui, |ui| {
                        for e in &snap.history {
                            row(ui, &p, shared, e);
                        }
                    });
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    if let Some(a) = footer(ui, &p, shared, &snap) {
                        action = a;
                    }
                    widgets::divider(ui, &p);
                });
            });
        action
    }

    fn header(&mut self, ui: &mut Ui, p: &Palette, snap: &Snapshot) -> Action {
        let mut action = Action::None;
        ui.horizontal(|ui| {
            widgets::logo(ui, p, 24.0);
            ui.label(widgets::strong("Clipshare Quick", p));
            widgets::badge(ui, p, snap.tray.label(), header_tone(snap.tray));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, p, "⚙", "設定").clicked() {
                    action = Action::Open(Screen::Settings(SettingsTab::Upload));
                }
            });
        });
        action
    }

    fn controls(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>, snap: &Snapshot) {
        widgets::card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(widgets::strong("自動アップロード", p));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // 403 の間は実際の状態（OFF）を表示し、操作できないようにする（デザイン 09、Q-031）。
                    let mut on = snap.auto_upload;
                    let changed = ui
                        .add_enabled_ui(!snap.blocked, |ui| {
                            widgets::toggle(ui, p, &mut on, "自動アップロード").changed()
                        })
                        .inner;
                    if changed {
                        shared.send(Command::SetAutoUpload(on));
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label(widgets::body("画質", p));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.quality = shared.engine.settings().get().default_quality;
                    let mut q = self.quality;
                    if widgets::segmented(
                        ui,
                        p,
                        &mut q,
                        &[(Quality::Original, "原寸"), (Quality::Shrink, "縮小")],
                    ) {
                        if let Err(e) = shared.engine.set_default_quality(q) {
                            tracing::warn!(error = %e, "画質を変更できませんでした");
                        }
                        self.quality = q;
                    }
                });
            });
        });
    }
}

fn anonymous_notice(ui: &mut Ui, p: &Palette, snap: &Snapshot) -> Option<Action> {
    let mut action = None;
    widgets::tinted(ui, p.was, p.was, |ui| {
        ui.horizontal(|ui| {
            ui.label(widgets::strong("未ログインで利用中", p));
            if let Some(l) = &snap.limits
                && let Some(limit) = l.daily()
            {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("今日 {} / {} 件", l.daily_used.min(limit), limit))
                            .font(bold(12.0))
                            .color(p.wa),
                    );
                });
            }
        });
        ui.label(widgets::muted(
            "画像にバナーが付きます。ログインすると制限が外れます。",
            p,
        ));
        // 403（BAN など）のときは再認証を促さない（Q-024）。
        if !snap.blocked && widgets::small(ui, p, "ログインして制限を解除").clicked() {
            action = Some(Action::Open(Screen::Settings(SettingsTab::Account)));
        }
    });
    action
}

/// ヘッダーの状態バッジの色。「利用不可」はデザイン 09 のとおり灰色（再試行で直るエラーではないため）。
pub fn header_tone(state: TrayState) -> Tone {
    match state {
        TrayState::Idle | TrayState::Uploading => Tone::Primary,
        TrayState::Error | TrayState::Offline => Tone::Danger,
        TrayState::Paused | TrayState::Blocked => Tone::Muted,
        TrayState::NeedsLogin => Tone::Warning,
    }
}

/// 共有URLの表示用（スキームを省く）。
pub fn short_url(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_owned()
}

fn row(ui: &mut Ui, p: &Palette, shared: &Arc<Shared>, e: &HistoryEntry) {
    let now = Utc::now();
    let status = e.display_status(now);
    ui.horizontal(|ui| {
        ui.set_min_height(44.0);
        // 種別のマーク（サムネイルの代わり）
        let (rect, _) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(6), p.mut_);
        let glyph = if e.kind == MediaKind::Video {
            "🎞"
        } else {
            "🖼"
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            regular(14.0),
            p.mfg,
        );

        let right_w = 96.0;
        let text_w = (ui.available_width() - right_w).max(80.0);
        ui.allocate_ui_with_layout(
            egui::vec2(text_w, 40.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&e.file_name).font(bold(13.0)).color(p.fg))
                        .truncate(),
                );
                let sub = subline(e, status, shared);
                ui.add(
                    egui::Label::new(RichText::new(sub).font(regular(11.5)).color(p.mfg))
                        .truncate(),
                );
                if status == DisplayStatus::Uploading {
                    let f = shared.engine.progress_of(&e.id).unwrap_or(0.0);
                    widgets::progress_bar(ui, p, f, text_w - 8.0);
                }
            },
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match status {
            DisplayStatus::Done => {
                if widgets::icon_button(ui, p, "🗑", "削除").clicked() {
                    shared.send(Command::DeleteEntry(e.id.clone()));
                }
                if widgets::icon_button(ui, p, "🌐", "ブラウザで開く").clicked()
                    && let Some(u) = &e.url
                {
                    shared.send(Command::OpenUrl(u.clone()));
                }
                if widgets::icon_button(ui, p, "📋", "URLをコピー").clicked()
                    && let Err(err) = shared.engine.copy_url(&e.id)
                {
                    tracing::warn!(error = %err, "コピーできませんでした");
                }
            }
            DisplayStatus::Processing => widgets::badge(ui, p, "処理中", Tone::Primary),
            DisplayStatus::Uploading => widgets::spinner(ui, p, 16.0),
            DisplayStatus::Queued | DisplayStatus::Waiting => {
                widgets::badge(ui, p, "待機中", Tone::Muted)
            }
            DisplayStatus::Failed => {
                let retryable = e.failure.as_ref().is_none_or(|f| f.retryable);
                if retryable {
                    if widgets::small(ui, p, "再試行").clicked()
                        && let Err(err) = shared.engine.retry(&e.id)
                    {
                        tracing::warn!(error = %err, "再試行できませんでした");
                    }
                } else if widgets::icon_button(ui, p, "✕", "履歴から消す").clicked() {
                    let _ = shared.engine.remove_entry(&e.id);
                }
                widgets::badge(ui, p, "失敗", Tone::Danger);
            }
            DisplayStatus::Expired | DisplayStatus::Deleted => {
                if widgets::icon_button(ui, p, "✕", "履歴から消す").clicked() {
                    let _ = shared.engine.remove_entry(&e.id);
                }
                let label = if status == DisplayStatus::Expired {
                    "期限切れ"
                } else {
                    "削除済み"
                };
                widgets::badge(ui, p, label, Tone::Muted);
            }
        });
    });
}

fn subline(e: &HistoryEntry, status: DisplayStatus, shared: &Arc<Shared>) -> String {
    let now = Utc::now();
    match status {
        DisplayStatus::Done => {
            let url = e.url.as_deref().map(short_url).unwrap_or_default();
            match e.remaining_label(now) {
                Some(r) => format!("{url} · {r}"),
                None => url,
            }
        }
        DisplayStatus::Processing => {
            let copied = e.source == EntrySource::Auto
                && shared.engine.settings().get().copy_url_after_upload;
            if copied {
                "変換中 · URLはコピー済み".to_owned()
            } else {
                "処理が終わると再生できます".to_owned()
            }
        }
        DisplayStatus::Uploading => {
            let pct = (shared.engine.progress_of(&e.id).unwrap_or(0.0) * 100.0).round() as u32;
            format!("{pct}%")
        }
        DisplayStatus::Waiting => "接続待ち".to_owned(),
        DisplayStatus::Queued => String::new(),
        DisplayStatus::Failed => e
            .failure
            .as_ref()
            .map(|f| f.message.clone())
            .unwrap_or_default(),
        DisplayStatus::Expired => "24時間が経過しました".to_owned(),
        DisplayStatus::Deleted => String::new(),
    }
}

fn footer(ui: &mut Ui, p: &Palette, shared: &Arc<Shared>, snap: &Snapshot) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        let (name, sub) = match &snap.account {
            AccountView::LoggedIn { name } => (name.clone(), "ログイン中・バナーなし".to_owned()),
            AccountView::NeedsRelogin { name } => (name.clone(), "連携が切れています".to_owned()),
            AccountView::Anonymous => {
                let limit = snap
                    .limits
                    .as_ref()
                    .and_then(|l| l.daily())
                    .map(|d| format!("バナーあり・1日{d}件"))
                    .unwrap_or_else(|| "バナーあり".to_owned());
                ("未ログイン".to_owned(), limit)
            }
        };
        widgets::avatar(ui, p, &name, 28.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(&name).font(bold(13.0)).color(p.fg));
            ui.label(RichText::new(sub).font(regular(11.0)).color(p.mfg));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if matches!(snap.account, AccountView::NeedsRelogin { .. }) {
                if widgets::small(ui, p, "再ログイン").clicked() {
                    shared.send(Command::Login);
                    action = Some(Action::Close);
                }
            } else if ui
                .add(
                    egui::Label::new(RichText::new("Webで管理").font(bold(12.0)).color(p.prt))
                        .sense(egui::Sense::click()),
                )
                .clicked()
            {
                shared.send(Command::OpenUrl(crate::actions::manage_url(shared)));
            }
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_badge_is_muted_like_design() {
        assert_eq!(header_tone(TrayState::Blocked), Tone::Muted);
        assert_eq!(header_tone(TrayState::Error), Tone::Danger);
        assert_eq!(header_tone(TrayState::Idle), Tone::Primary);
    }

    #[test]
    fn short_url_strips_scheme() {
        assert_eq!(
            short_url("https://clipshare.link/q/aB3dE9xYz1"),
            "clipshare.link/q/aB3dE9xYz1"
        );
    }
}
