//! 初回セットアップ（画面 04・05・06）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use cq_core::vrchat::Detection;
use egui::{Align, Layout, RichText, Ui};

use super::theme::{Palette, bold, regular};
use super::widgets::{self, Tone};
use crate::app::{LoginState, Shared};
use crate::host::Action;

#[derive(Debug)]
pub struct Wizard {
    step: usize,
    auto_upload: bool,
    detection: Detection,
    folder: Option<PathBuf>,
    started_login: bool,
}

impl Wizard {
    pub fn new(shared: &Arc<Shared>) -> Self {
        let detection = cq_core::vrchat::detect_current();
        let folder = detection.path().map(PathBuf::from).or_else(|| {
            shared
                .engine
                .settings()
                .get()
                .watch_folders
                .first()
                .cloned()
        });
        Self {
            step: 1,
            auto_upload: true,
            detection,
            folder,
            started_login: false,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, shared: &Arc<Shared>) -> Action {
        let p = shared.palette;
        let mut action = Action::None;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(32, 24))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                widgets::steps(ui, &p, self.step);
                ui.add_space(16.0);
                action = match self.step {
                    1 => self.welcome(ui, &p),
                    2 => self.folder(ui, &p),
                    _ => self.auth(ui, &p, shared),
                };
            });
        if let Action::Close = action {
            self.finish(shared);
        }
        action
    }

    /// 設定を保存してセットアップを終える。
    fn finish(&self, shared: &Arc<Shared>) {
        let folder = self.folder.clone();
        let auto = self.auto_upload;
        let r = shared.engine.settings().update(|s| {
            s.onboarding_completed = true;
            s.auto_upload_consented = auto;
            s.auto_upload = auto;
            if let Some(f) = folder
                && !s
                    .watch_folders
                    .iter()
                    .any(|x| cq_core::settings::folder_key(x) == cq_core::settings::folder_key(&f))
            {
                s.watch_folders.insert(0, f);
            }
        });
        if let Err(e) = r {
            tracing::error!(error = %e, "初回設定を保存できませんでした");
        } else {
            tracing::info!(auto, "初回セットアップを完了しました");
        }
        shared.reset_login();
    }

    fn welcome(&mut self, ui: &mut Ui, p: &Palette) -> Action {
        ui.label(widgets::title("ようこそ", p));
        ui.label(widgets::body(
            "VRChatで撮った写真を、撮影と同時に共有URLにします。URLはクリップボードに自動でコピーされ、チャットやSNSにそのまま貼り付けられます。",
            p,
        ));
        ui.add_space(8.0);
        widgets::card(ui, p, |ui| {
            ui.label(widgets::strong("自動アップロードについて", p));
            for line in [
                "・撮影した写真は Clipshare に送信され、共有URLが発行されます。",
                "・URLを知っている人は誰でも写真を見られます。保存期間は24時間で、過ぎると自動で削除されます。",
                "・撮影情報などの埋め込みデータは、アップロード時に取り除かれます。",
            ] {
                ui.label(RichText::new(line).font(regular(13.0)).color(p.mfg));
            }
        });
        bottom_bar(ui, p, |ui| {
            if widgets::primary(ui, p, "自動アップロードを有効にして続ける").clicked()
            {
                self.auto_upload = true;
                self.step = 2;
            }
            if widgets::secondary(ui, p, "手動でのみ使う").clicked() {
                self.auto_upload = false;
                self.step = 2;
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(widgets::muted("あとから設定で変更できます", p));
            });
        });
        Action::None
    }

    fn folder(&mut self, ui: &mut Ui, p: &Palette) -> Action {
        ui.label(widgets::title("写真の保存先", p));
        ui.label(widgets::body(
            "このフォルダに保存された新しい写真を監視します。",
            p,
        ));
        ui.add_space(8.0);
        let detected =
            self.detection.path().is_some() && self.folder.as_deref() == self.detection.path();
        widgets::card(ui, p, |ui| {
            ui.horizontal(|ui| {
                if detected {
                    widgets::badge(ui, p, "検出しました", Tone::Primary);
                    ui.label(widgets::muted("VRChatの写真フォルダ", p));
                } else if self.folder.is_none() {
                    widgets::badge(ui, p, "見つかりませんでした", Tone::Warning);
                }
            });
            ui.horizontal(|ui| {
                let shown = self
                    .folder
                    .as_ref()
                    .map(|f| f.display().to_string())
                    .unwrap_or_default();
                let w = ui.available_width() - 80.0;
                egui::Frame::new()
                    .fill(p.mut_)
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.add(
                            egui::Label::new(RichText::new(shown).font(regular(13.0)).color(p.fg))
                                .truncate(),
                        )
                        .on_hover_text("監視するフォルダ");
                    });
                let label = if self.folder.is_some() {
                    "変更"
                } else {
                    "選択"
                };
                if widgets::secondary(ui, p, label).clicked()
                    && let Some(f) = crate::platform::pick_folder(self.folder.as_deref())
                {
                    self.folder = Some(f);
                }
            });
            let note = if self.folder.is_some() {
                "年月ごとのサブフォルダも含めて監視します。すでにある写真はアップロードしません。"
            } else {
                "VRChatの写真の保存先を変更している場合は、そのフォルダを選んでください。"
            };
            ui.label(widgets::muted(note, p));
        });
        bottom_bar(ui, p, |ui| {
            let can_next = self.folder.is_some() || !self.auto_upload;
            if widgets::primary_enabled(ui, p, "次へ", can_next).clicked() {
                self.step = 3;
            }
            if widgets::secondary(ui, p, "戻る").clicked() {
                self.step = 1;
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(widgets::muted("手順 2 / 3", p));
            });
        });
        Action::None
    }

    fn auth(&mut self, ui: &mut Ui, p: &Palette, shared: &Arc<Shared>) -> Action {
        if !self.started_login {
            self.started_login = true;
            if matches!(
                shared.engine.snapshot().borrow().account,
                cq_core::engine::AccountView::Anonymous
            ) {
                shared.start_login();
            }
        }
        let mut action = Action::None;
        match shared.login_state() {
            LoginState::Idle | LoginState::Starting => {
                ui.label(widgets::title("Clipshareと連携", p));
                ui.label(widgets::body(
                    "ブラウザが開きます。Clipshareにログインして、このアプリを許可してください。",
                    p,
                ));
                ui.add_space(8.0);
                widgets::spinner(ui, p, 20.0);
                if let LoginState::Idle = shared.login_state()
                    && let cq_core::engine::AccountView::LoggedIn { name } =
                        shared.engine.snapshot().borrow().account.clone()
                {
                    return self.success(ui, p, &name);
                }
                bottom_bar(ui, p, |ui| {
                    if widgets::secondary(ui, p, "あとで連携する").clicked() {
                        shared.cancel_login();
                        action = Action::Close;
                    }
                });
            }
            LoginState::Waiting {
                code,
                url,
                deadline,
            } => {
                ui.label(widgets::title("Clipshareと連携", p));
                ui.label(widgets::body(
                    "ブラウザが開きます。Clipshareにログインして、このアプリを許可してください。",
                    p,
                ));
                ui.add_space(8.0);
                connection_code_card(ui, p, &code, deadline);
                ui.horizontal(|ui| {
                    if widgets::small(ui, p, "ブラウザを開き直す").clicked() {
                        crate::platform::open_url(&url);
                    }
                });
                ui.label(widgets::muted(
                    "連携しない場合は、バナー付き・1日5件までで使えます。",
                    p,
                ));
                bottom_bar(ui, p, |ui| {
                    if widgets::secondary(ui, p, "あとで連携する").clicked() {
                        shared.cancel_login();
                        action = Action::Close;
                    }
                });
            }
            LoginState::Success { name } => {
                action = self.success(ui, p, &name);
            }
            LoginState::Failed { code } => {
                let next = failure_action(&code);
                ui.label(widgets::title("連携できませんでした", p));
                ui.label(widgets::body(login_failure_text(&code), p));
                ui.add_space(8.0);
                widgets::card(ui, p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(widgets::muted("エラーコード", p));
                        ui.label(RichText::new(code).font(bold(13.0)).color(p.det));
                    });
                });
                ui.label(widgets::muted(
                    "解決しない場合は、設定の「一般」からログを開いて確認できます。",
                    p,
                ));
                bottom_bar(ui, p, |ui| {
                    match next {
                        // 403 は再認可で解決しないため、再試行ではなくお問い合わせへ案内する（Q-024）。
                        FailureAction::Contact => {
                            if widgets::primary(ui, p, next.label()).clicked() {
                                crate::platform::open_url(&crate::actions::contact_url(shared));
                            }
                        }
                        FailureAction::Retry => {
                            if widgets::primary(ui, p, next.label()).clicked() {
                                shared.reset_login();
                                shared.start_login();
                            }
                        }
                    }
                    if widgets::secondary(ui, p, "あとで連携する").clicked() {
                        shared.reset_login();
                        action = Action::Close;
                    }
                });
            }
        }
        action
    }

    fn success(&mut self, ui: &mut Ui, p: &Palette, name: &str) -> Action {
        let mut action = Action::None;
        ui.label(widgets::title("連携しました", p));
        ui.label(widgets::body(
            "このPCのアプリが、あなたのClipshareアカウントでアップロードできるようになりました。",
            p,
        ));
        ui.add_space(8.0);
        widgets::card(ui, p, |ui| {
            ui.horizontal(|ui| {
                widgets::avatar(ui, p, name, 36.0);
                ui.vertical(|ui| {
                    ui.label(widgets::strong(name, p));
                    ui.label(widgets::muted("ログイン中", p));
                });
            });
            ui.label(
                RichText::new("✓ 画像にバナーは付きません")
                    .font(regular(13.0))
                    .color(p.prt),
            );
            ui.label(
                RichText::new("✓ 保存期間は24時間です")
                    .font(regular(13.0))
                    .color(p.prt),
            );
        });
        bottom_bar(ui, p, |ui| {
            if widgets::primary(ui, p, "完了して始める").clicked() {
                action = Action::Close;
            }
        });
        action
    }
}

/// 連携に失敗したときの主ボタン（画面 06 の「失敗」各状態）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureAction {
    /// 403: 「お問い合わせ」（再試行は出さない）。
    Contact,
    /// それ以外（キャンセル・時間切れ・503 など）: 「もう一度試す」。
    Retry,
}

impl FailureAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Contact => "お問い合わせ",
            Self::Retry => "もう一度試す",
        }
    }
}

/// エラーコードから主ボタンを決める。
pub fn failure_action(code: &str) -> FailureAction {
    if code == "FORBIDDEN" {
        FailureAction::Contact
    } else {
        FailureAction::Retry
    }
}

/// 連携の失敗の説明。403（BAN）と 503（発行停止中）は再試行やキャンセルとは別の案内にする（契約 v1.1）。
pub fn login_failure_text(code: &str) -> &'static str {
    match code {
        "FORBIDDEN" => "このアカウントではClipshare Quickを利用できません。",
        "SERVICE_UNAVAILABLE" => {
            "現在、新しい連携を受け付けていません。時間をおいてお試しください。"
        }
        _ => "許可がキャンセルされたか、時間切れになりました。もう一度お試しください。",
    }
}

/// 接続コードと残り時間（画面 06）。
pub fn connection_code_card(ui: &mut Ui, p: &Palette, code: &str, deadline: Instant) {
    widgets::card(ui, p, |ui| {
        ui.label(widgets::muted("接続コード", p));
        ui.label(RichText::new(code).font(bold(30.0)).color(p.fg));
        ui.label(widgets::muted(
            "ブラウザの画面に同じコードが表示されていることを確認してください。",
            p,
        ));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            widgets::spinner(ui, p, 14.0);
            ui.label(widgets::body("ブラウザでの許可を待っています…", p));
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(widgets::muted(
                    &format!("残り {}:{:02}", left / 60, left % 60),
                    p,
                ));
            });
        });
    });
}

/// 下部のボタン列（右寄せ）。
fn bottom_bar(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui)) {
    widgets::bottom_bar(ui, p, add);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 画面 06「失敗（403）」: お問い合わせのみ（もう一度試すは出さない）。
    #[test]
    fn forbidden_failure_points_to_contact() {
        assert_eq!(failure_action("FORBIDDEN"), FailureAction::Contact);
        assert_eq!(FailureAction::Contact.label(), "お問い合わせ");
        assert_eq!(
            login_failure_text("FORBIDDEN"),
            "このアカウントではClipshare Quickを利用できません。"
        );
    }

    /// 画面 06「失敗（503）」: 発行停止の説明と「もう一度試す」。
    #[test]
    fn unavailable_failure_can_retry() {
        assert_eq!(failure_action("SERVICE_UNAVAILABLE"), FailureAction::Retry);
        assert_eq!(FailureAction::Retry.label(), "もう一度試す");
        assert_eq!(
            login_failure_text("SERVICE_UNAVAILABLE"),
            "現在、新しい連携を受け付けていません。時間をおいてお試しください。"
        );
    }

    /// 画面 06「失敗・キャンセル」: 従来どおり。
    #[test]
    fn timeout_failure_can_retry() {
        assert_eq!(failure_action("AUTH_TIMEOUT"), FailureAction::Retry);
        assert_eq!(
            login_failure_text("AUTH_TIMEOUT"),
            "許可がキャンセルされたか、時間切れになりました。もう一度お試しください。"
        );
    }
}
