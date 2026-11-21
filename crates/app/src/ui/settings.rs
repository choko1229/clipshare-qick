//! 設定（画面 10・11・12 と、通知・情報タブ）。
//!
//! 変更はその場で保存して反映する（再起動不要）。不正値は保存前に弾かれる（[`cq_core::settings::Settings::validate`]）。

use std::sync::Arc;

use cq_core::engine::AccountView;
use cq_core::settings::{Quality, Settings};
use egui::{Align, Layout, RichText, Ui};

use super::theme::{Palette, bold, regular};
use super::widgets::{self, Tone};
use crate::app::{LoginState, Screen, SettingsTab, Shared};
use crate::host::Action;

#[derive(Debug)]
pub struct SettingsWindow {
    tab: SettingsTab,
    error: Option<String>,
    copied_at: Option<std::time::Instant>,
}

impl SettingsWindow {
    pub fn new(tab: SettingsTab) -> Self {
        Self {
            tab,
            error: None,
            copied_at: None,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, shared: &Arc<Shared>) -> Action {
        let p = shared.palette;
        let mut action = Action::None;
        // サイドバー（タブ）
        egui::SidePanel::left("settings-nav")
            .resizable(false)
            .exact_width(200.0)
            .frame(
                egui::Frame::new()
                    .fill(p.card)
                    .inner_margin(egui::Margin::symmetric(12, 16)),
            )
            .show_inside(ui, |ui| {
                for t in SettingsTab::ALL {
                    let sel = t == self.tab;
                    let b = egui::Button::new(
                        RichText::new(t.label()).font(bold(14.0)).color(if sel {
                            p.fg
                        } else {
                            p.mfg
                        }),
                    )
                    .fill(if sel {
                        p.mut_
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(egui::CornerRadius::same(8))
                    .min_size(egui::vec2(176.0, 36.0));
                    if ui.add(b).clicked() {
                        self.tab = t;
                        self.error = None;
                    }
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(28, 24)),
            )
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(widgets::title(self.tab.label(), &p));
                        ui.add_space(8.0);
                        action = match self.tab {
                            SettingsTab::Upload => self.upload(ui, &p, shared),
                            SettingsTab::Account => self.account(ui, &p, shared),
                            SettingsTab::VrChat => self.vrchat(ui, &p, shared),
                            SettingsTab::Notifications => self.notifications(ui, &p, shared),
                            SettingsTab::General => self.general(ui, &p, shared),
                            SettingsTab::About => self.about(ui, &p, shared),
                        };
                        if let Some(e) = &self.error {
                            widgets::tinted(ui, p.des, p.des, |ui| {
                                ui.label(RichText::new(e).font(regular(13.0)).color(p.det));
                            });
                        }
                    });
            });
        action
    }

    fn save(&mut self, shared: &Arc<Shared>, f: impl FnOnce(&mut Settings)) {
        match shared.engine.settings().update(f) {
            Ok(_) => self.error = None,
            Err(e) => {
                tracing::warn!(error = %e, "設定を保存できませんでした");
                self.error = Some(e.to_string());
            }
        }
    }

    fn upload(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        let s = shared.engine.settings().get();
        widgets::card(ui, p, |ui| {
            let mut on = s.auto_upload_active();
            if setting_row(
                ui,
                p,
                "自動アップロード",
                "VRChatで撮影した写真を自動でアップロードします。",
                |ui| widgets::toggle(ui, p, &mut on, "自動アップロード").changed(),
            ) && let Err(e) = shared.engine.set_auto_upload(on)
            {
                self.error = Some(e.to_string());
            }
            widgets::divider(ui, p);
            let mut copy = s.copy_url_after_upload;
            if setting_row(
                ui,
                p,
                "撮影後にURLをコピー",
                "アップロードが終わったら、共有URLをクリップボードに入れます。",
                |ui| widgets::toggle(ui, p, &mut copy, "撮影後にURLをコピー").changed(),
            ) {
                self.save(shared, |x| x.copy_url_after_upload = copy);
            }
        });
        ui.add_space(8.0);
        ui.label(widgets::heading("既定の画質", p));
        for (q, title, desc) in [
            (
                Quality::Original,
                "原寸",
                "元の解像度のまま、高画質WebPで保存します。埋め込み情報は取り除かれます。",
            ),
            (
                Quality::Shrink,
                "縮小（長辺2000px）",
                "WebPで軽量化します。VRChat内の画像貼り付けギミック向けです。",
            ),
        ] {
            let selected = s.default_quality == q;
            let fill = if selected { p.prs } else { p.card };
            let stroke = if selected { p.pr } else { p.bd };
            let r = egui::Frame::new()
                .fill(fill)
                .stroke(egui::Stroke::new(1.0_f32, stroke))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let _ = ui.radio(selected, "");
                        ui.vertical(|ui| {
                            ui.label(widgets::strong(title, p));
                            ui.label(widgets::muted(desc, p));
                        });
                    });
                })
                .response
                .interact(egui::Sense::click());
            if r.clicked()
                && !selected
                && let Err(e) = shared.engine.set_default_quality(q)
            {
                self.error = Some(e.to_string());
            }
        }
        let snap = shared.engine.snapshot().borrow().clone();
        let banner_note = match snap.account {
            AccountView::LoggedIn { .. } => {
                "ログイン中のため、画像にバナーは付きません。保存期間は24時間です。"
            }
            _ => "未ログインのため、画像にバナーが付きます。保存期間は24時間です。",
        };
        ui.label(widgets::muted(banner_note, p));
        ui.label(widgets::muted(
            "右クリックから共有するときは、メニューで画質を選べます。",
            p,
        ));
        Action::None
    }

    fn account(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        let snap = shared.engine.snapshot().borrow().clone();
        let login = shared.login_state();
        if let LoginState::Waiting {
            code,
            url,
            deadline,
        } = &login
        {
            super::wizard::connection_code_card(ui, p, code, *deadline);
            ui.horizontal(|ui| {
                if widgets::small(ui, p, "ブラウザを開き直す").clicked() {
                    crate::platform::open_url(url);
                }
                if widgets::small(ui, p, "キャンセル").clicked() {
                    shared.cancel_login();
                }
            });
            return Action::None;
        }
        if let LoginState::Failed { code } = &login {
            widgets::tinted(ui, p.des, p.des, |ui| {
                ui.label(widgets::strong("連携できませんでした", p));
                ui.label(widgets::muted(super::wizard::login_failure_text(code), p));
                if code == "FORBIDDEN" && widgets::small(ui, p, "お問い合わせ").clicked() {
                    crate::platform::open_url(&crate::actions::contact_url(shared));
                }
                ui.horizontal(|ui| {
                    ui.label(widgets::muted("エラーコード", p));
                    ui.label(RichText::new(code).font(bold(13.0)).color(p.det));
                });
            });
        }
        if snap.blocked {
            widgets::tinted(ui, p.des, p.des, |ui| {
                ui.label(widgets::strong("このアカウントでは利用できません", p));
                if widgets::small(ui, p, "お問い合わせ").clicked() {
                    crate::platform::open_url(&crate::actions::contact_url(shared));
                }
            });
        }
        match &snap.account {
            AccountView::LoggedIn { name } | AccountView::NeedsRelogin { name } => {
                let broken = matches!(snap.account, AccountView::NeedsRelogin { .. });
                widgets::card(ui, p, |ui| {
                    ui.horizontal(|ui| {
                        widgets::avatar(ui, p, name, 40.0);
                        ui.vertical(|ui| {
                            ui.label(widgets::strong(name, p));
                            if broken {
                                widgets::badge(ui, p, "連携が切れています", Tone::Warning);
                            } else {
                                ui.label(widgets::muted("連携済み · このPC（Windows）", p));
                            }
                        });
                    });
                    widgets::divider(ui, p);
                    if let Some(l) = &snap.limits {
                        stat_row(ui, p, "今日のアップロード", &format!("{} 件", l.daily_used));
                        let limit = l
                            .daily()
                            .map(|d| format!("{d} 件"))
                            .unwrap_or_else(|| "上限なし".to_owned());
                        stat_row(ui, p, "1日の上限", &limit);
                        stat_row(
                            ui,
                            p,
                            "画像のバナー",
                            if l.banner { "あり" } else { "なし" },
                        );
                    }
                });
                ui.horizontal(|ui| {
                    if broken && widgets::primary(ui, p, "再ログイン").clicked() {
                        shared.start_login();
                    }
                    if widgets::secondary(ui, p, "連携アプリをWebで管理").clicked() {
                        crate::platform::open_url(&crate::actions::manage_url(shared));
                    }
                    if widgets::secondary(ui, p, "ログアウト").clicked() {
                        let s = Arc::clone(shared);
                        shared.rt.spawn(async move {
                            match s.engine.logout().await {
                                Ok(true) => {}
                                Ok(false) => crate::toast::show_simple(
                                    &s,
                                    "連携の解除が完了していません",
                                    "Clipshareの「連携アプリ」からこのPCを解除してください。",
                                ),
                                Err(e) => tracing::warn!(error = %e, "ログアウトできませんでした"),
                            }
                            s.reset_login();
                            s.wake();
                        });
                    }
                });
                ui.label(widgets::muted(
                    "ログアウトしても、アップロード済みの写真は期限まで残ります。",
                    p,
                ));
            }
            AccountView::Anonymous => {
                widgets::tinted(ui, p.was, p.was, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(widgets::strong("未ログインで利用中", p));
                        if let Some(l) = snap.limits.as_ref()
                            && let Some(d) = l.daily()
                        {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!("今日 {} / {d} 件", l.daily_used.min(d)))
                                        .font(bold(12.0))
                                        .color(p.wa),
                                );
                            });
                        }
                    });
                    for l in [
                        "・画像にバナーが付きます",
                        "・1日5件までアップロードできます",
                        "・履歴の削除はこのPCからのみできます",
                    ] {
                        ui.label(RichText::new(l).font(regular(13.0)).color(p.fg));
                    }
                });
                ui.add_space(8.0);
                // 403（BAN など）のときは再認証を促さない（Q-024）。
                let forbidden = snap.blocked
                    || matches!(&login, LoginState::Failed { code } if code == "FORBIDDEN");
                if !forbidden {
                    if widgets::primary(ui, p, "ログインして連携").clicked() {
                        shared.start_login();
                    }
                    ui.label(widgets::muted(
                        "ブラウザが開きます。許可するとこのアプリでアップロードできるようになります。",
                        p,
                    ));
                }
            }
        }
        Action::None
    }

    fn vrchat(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        let s = shared.engine.settings().get();
        let snap = shared.engine.snapshot().borrow().clone();
        ui.label(widgets::body(
            "このフォルダに保存された新しい写真を監視します。",
            p,
        ));
        let mut remove: Option<usize> = None;
        for (i, f) in s.watch_folders.iter().enumerate() {
            widgets::card(ui, p, |ui| {
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 110.0;
                    ui.allocate_ui(egui::vec2(w, 24.0), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(f.display().to_string())
                                    .font(regular(13.0))
                                    .color(p.fg),
                            )
                            .truncate(),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, p, "✕", "このフォルダを外す").clicked()
                        {
                            remove = Some(i);
                        }
                        let watching = snap.folders.iter().any(|st| {
                            st.watching
                                && cq_core::settings::folder_key(&st.folder)
                                    == cq_core::settings::folder_key(f)
                        });
                        if watching {
                            widgets::badge(ui, p, "監視中", Tone::Primary);
                        } else {
                            widgets::badge(ui, p, "見つかりませんでした", Tone::Warning);
                        }
                    });
                });
            });
        }
        if let Some(i) = remove {
            self.save(shared, |x| {
                if i < x.watch_folders.len() {
                    x.watch_folders.remove(i);
                }
            });
        }
        if widgets::secondary(ui, p, "フォルダを追加").clicked()
            && let Some(f) =
                crate::platform::pick_folder(s.watch_folders.first().map(|x| x.as_path()))
        {
            self.save(shared, |x| x.watch_folders.push(f));
        }
        ui.label(widgets::muted(
            "すでにある写真はアップロードしません。書き込みが終わった新しい写真だけを送ります。",
            p,
        ));
        ui.add_space(8.0);
        widgets::card(ui, p, |ui| {
            let mut v = s.watch_videos;
            if setting_row(ui, p, "動画も自動でアップロード", "", |ui| {
                widgets::toggle(ui, p, &mut v, "動画も自動でアップロード").changed()
            }) {
                self.save(shared, |x| x.watch_videos = v);
            }
        });
        Action::None
    }

    fn notifications(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        let s = shared.engine.settings().get();
        widgets::card(ui, p, |ui| {
            let mut t = s.notifications.windows_toast;
            if setting_row(ui, p, "Windowsの通知", "", |ui| {
                widgets::toggle(ui, p, &mut t, "Windowsの通知").changed()
            }) {
                self.save(shared, |x| x.notifications.windows_toast = t);
            }
            widgets::divider(ui, p);
            let mut v = s.notifications.vr_hud;
            if setting_row(ui, p, "VR内の通知", "", |ui| {
                widgets::toggle(ui, p, &mut v, "VR内の通知").changed()
            }) {
                self.save(shared, |x| x.notifications.vr_hud = v);
            }
        });
        Action::None
    }

    fn general(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        let s = shared.engine.settings().get();
        let mut action = Action::None;
        widgets::card(ui, p, |ui| {
            let mut st = s.launch_at_startup;
            if setting_row(
                ui,
                p,
                "Windows起動時に開始",
                "ログイン後、バックグラウンドで常駐します。",
                |ui| widgets::toggle(ui, p, &mut st, "Windows起動時に開始").changed(),
            ) {
                self.save(shared, |x| x.launch_at_startup = st);
            }
            widgets::divider(ui, p);
            let mut up = s.auto_update_check;
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(widgets::strong("更新を自動で確認", p));
                    ui.label(widgets::muted(
                        "新しいバージョンがあるとお知らせします。",
                        p,
                    ));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    changed = widgets::toggle(ui, p, &mut up, "更新を自動で確認").changed();
                    ui.label(widgets::muted(&format!("v{}", cq_core::APP_VERSION), p));
                });
            });
            if changed {
                self.save(shared, |x| x.auto_update_check = up);
            }
            widgets::divider(ui, p);
            let mut cm = s.context_menu_enabled;
            if setting_row(ui, p, "右クリックメニュー", "", |ui| {
                widgets::toggle(ui, p, &mut cm, "右クリックメニュー").changed()
            }) {
                self.save(shared, |x| x.context_menu_enabled = cm);
            }
            widgets::divider(ui, p);
            let mut vr = s.steamvr_enabled;
            if setting_row(ui, p, "SteamVR", "", |ui| {
                widgets::toggle(ui, p, &mut vr, "SteamVR").changed()
            }) {
                self.save(shared, |x| x.steamvr_enabled = vr);
            }
        });
        ui.add_space(8.0);
        widgets::card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(widgets::strong("ログ", p));
                    ui.label(widgets::muted("エラーの調査に使えます。", p));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::secondary(ui, p, "ログを開く").clicked() {
                        crate::actions::open_logs(shared);
                    }
                });
            });
            widgets::divider(ui, p);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(widgets::strong("診断情報", p));
                    ui.label(widgets::muted(
                        "問い合わせ用に、バージョンと設定の概要をコピーします。",
                        p,
                    ));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if self.copied_at.is_some_and(|t| t.elapsed().as_secs() < 2) {
                        "✓"
                    } else {
                        "コピー"
                    };
                    if widgets::secondary(ui, p, label).clicked() {
                        let text = crate::actions::diagnostics(shared);
                        if crate::clipboard::SystemClipboard.set(&text) {
                            self.copied_at = Some(std::time::Instant::now());
                        }
                    }
                });
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if widgets::secondary(ui, p, "更新を確認").clicked() {
                crate::updater::check_now(shared, true);
                action = Action::Open(Screen::Update);
            }
            if widgets::secondary(ui, p, "アンインストール").clicked() {
                action = Action::Open(Screen::Uninstall);
            }
        });
        action
    }

    fn about(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        widgets::card(ui, p, |ui| {
            ui.horizontal(|ui| {
                widgets::logo(ui, p, 36.0);
                ui.vertical(|ui| {
                    ui.label(widgets::strong("Clipshare Quick", p));
                    ui.label(widgets::muted(
                        &format!("バージョン {}", cq_core::APP_VERSION),
                        p,
                    ));
                });
            });
            widgets::divider(ui, p);
            stat_row(ui, p, "ライセンス", "MIT");
            ui.horizontal(|ui| {
                if widgets::small(ui, p, "ソースコード").clicked() {
                    crate::platform::open_url("https://github.com/choko1229/clipshare-qick");
                }
                if widgets::small(ui, p, "更新履歴").clicked() {
                    crate::platform::open_url(&crate::actions::download_page_url(shared));
                }
            });
        });
        Action::None
    }
}

/// 見出し＋説明＋右側の操作の 1 行。右側が変更を返したら `true`。
fn setting_row(
    ui: &mut Ui,
    p: &Palette,
    title: &str,
    desc: &str,
    right: impl FnOnce(&mut Ui) -> bool,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(widgets::strong(title, p));
            if !desc.is_empty() {
                ui.label(widgets::muted(desc, p));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed = right(ui);
        });
    });
    changed
}

fn stat_row(ui: &mut Ui, p: &Palette, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(widgets::muted(label, p));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(widgets::strong(value, p));
        });
    });
}
