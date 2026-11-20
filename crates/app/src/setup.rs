//! セットアップ（インストーラー画面 02・03）。
//!
//! MSIX は Windows（App Installer / PackageManager）がインストールする。このセットアップは
//! 画面 02・03 の流れ（ようこそ → 使用許諾 → オプション → 署名の確認 → インストール → 完了/失敗）を表示し、
//! 同じフォルダの `.msix` を PackageManager でインストールしてから、オプションを初期設定として書き込む。
//! インストール先は Windows が管理するため、画面に出さない。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cq_core::paths::AppPaths;
use cq_core::settings::SettingsStore;
use egui::{RichText, Ui};

use crate::ui::theme::{self, Palette, bold, regular};
use crate::ui::widgets::{self, Tone};

/// インストールのオプション（画面 02 ③）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupOptions {
    pub launch_at_startup: bool,
    pub context_menu: bool,
    pub steamvr: bool,
    pub desktop_shortcut: bool,
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            launch_at_startup: true,
            context_menu: true,
            steamvr: true,
            desktop_shortcut: false,
        }
    }
}

/// インストールの手順（画面 03 ⑤）。
pub const INSTALL_STEPS: [&str; 5] = [
    "ファイルを展開中",
    "アプリ本体を配置",
    "右クリックメニュー拡張を登録",
    "VRChat写真フォルダを確認",
    "SteamVRに登録",
];

#[derive(Debug, Clone, PartialEq)]
enum Page {
    Welcome,
    License,
    Options,
    Trust,
    Installing { step: usize, percent: f32 },
    Done,
    Failed { step: usize, code: String },
}

/// 署名の情報（自己署名のときに表示する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerInfo {
    pub publisher: String,
    pub fingerprint: String,
}

/// 初期設定を書き込む（既存の設定は残し、オプションの項目だけ反映する）。
pub fn apply_options(
    paths: &AppPaths,
    opts: &SetupOptions,
    detected_folder: Option<&Path>,
) -> Result<(), String> {
    std::fs::create_dir_all(paths.root()).map_err(|e| e.to_string())?;
    let store = SettingsStore::load(&paths.settings_file());
    store
        .update(|s| {
            s.launch_at_startup = opts.launch_at_startup;
            s.context_menu_enabled = opts.context_menu;
            s.steamvr_enabled = opts.steamvr;
            if let Some(f) = detected_folder {
                if !s
                    .watch_folders
                    .iter()
                    .any(|x| cq_core::settings::folder_key(x) == cq_core::settings::folder_key(f))
                {
                    s.watch_folders.insert(0, f.to_path_buf());
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let flag = paths.shell_flag_file();
    if opts.context_menu {
        let _ = std::fs::remove_file(flag);
    } else {
        std::fs::write(flag, b"1").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// セットアップと同じフォルダにある `.msix`。
pub fn find_package(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("msix"))
        })
        .collect();
    found.sort();
    found.pop()
}

struct Shared {
    page: Page,
}

/// セットアップの画面。
pub struct SetupApp {
    palette: Palette,
    opts: SetupOptions,
    agreed: bool,
    trust: bool,
    package: Option<PathBuf>,
    cert: Option<PathBuf>,
    signer: Option<SignerInfo>,
    signed_ok: bool,
    launch_after: bool,
    state: Arc<Mutex<Shared>>,
}

impl std::fmt::Debug for SetupApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupApp").finish_non_exhaustive()
    }
}

impl SetupApp {
    pub fn new(dir: &Path) -> Self {
        let package = find_package(dir);
        let cert = std::fs::read_dir(dir).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cer")))
        });
        let signed_ok = package
            .as_deref()
            .is_some_and(|p| crate::packaging::verify_signature(p).is_ok());
        let signer = cert.as_deref().and_then(crate::packaging::certificate_info);
        Self {
            palette: theme::system_palette(),
            opts: SetupOptions::default(),
            agreed: false,
            trust: false,
            package,
            cert,
            signer,
            signed_ok,
            launch_after: true,
            state: Arc::new(Mutex::new(Shared {
                page: Page::Welcome,
            })),
        }
    }

    pub fn palette(&self) -> Palette {
        self.palette
    }

    fn page(&self) -> Page {
        self.state
            .lock()
            .map(|g| g.page.clone())
            .unwrap_or(Page::Welcome)
    }

    fn set_page(&self, p: Page) {
        if let Ok(mut g) = self.state.lock() {
            g.page = p;
        }
    }

    fn header(&self, ui: &mut Ui) {
        let p = &self.palette;
        let current = match self.page() {
            Page::Welcome => 0,
            Page::License => 1,
            Page::Options | Page::Trust => 2,
            Page::Installing { .. } | Page::Failed { .. } => 3,
            Page::Done => 4,
        };
        ui.horizontal(|ui| {
            for (i, label) in ["ようこそ", "使用許諾", "オプション", "インストール", "完了"]
                .iter()
                .enumerate()
            {
                let color = if i == current {
                    p.fg
                } else if i < current {
                    p.prt
                } else {
                    p.mfg
                };
                ui.label(RichText::new(*label).font(bold(12.0)).color(color));
                if i < 4 {
                    ui.label(RichText::new("›").font(regular(12.0)).color(p.mfg));
                }
            }
        });
        ui.add_space(12.0);
    }

    fn install(&self, ctx: egui::Context) {
        let Some(pkg) = self.package.clone() else {
            self.set_page(Page::Failed {
                step: 0,
                code: "PACKAGE_NOT_FOUND".into(),
            });
            return;
        };
        let opts = self.opts;
        let state = Arc::clone(&self.state);
        let trust_cert = (self.trust && !self.signed_ok)
            .then(|| self.cert.clone())
            .flatten();
        std::thread::spawn(move || {
            let set = |p: Page| {
                if let Ok(mut g) = state.lock() {
                    g.page = p;
                }
                ctx.request_repaint();
            };
            if let Some(cer) = trust_cert {
                if let Err(code) = crate::packaging::trust_certificate(&cer) {
                    set(Page::Failed { step: 0, code });
                    return;
                }
            }
            set(Page::Installing {
                step: 0,
                percent: 0.0,
            });
            let progress = {
                let state = Arc::clone(&state);
                let ctx = ctx.clone();
                move |pct: f32| {
                    let step = if pct < 60.0 {
                        0
                    } else if pct < 90.0 {
                        1
                    } else {
                        2
                    };
                    if let Ok(mut g) = state.lock() {
                        g.page = Page::Installing { step, percent: pct };
                    }
                    ctx.request_repaint();
                }
            };
            if let Err(e) = crate::packaging::install_package(&pkg, progress) {
                tracing::error!(error = %e, "パッケージをインストールできませんでした");
                set(Page::Failed {
                    step: 2,
                    code: e.to_string(),
                });
                return;
            }
            set(Page::Installing {
                step: 3,
                percent: 100.0,
            });
            let detected = cq_core::vrchat::detect_current();
            set(Page::Installing {
                step: 4,
                percent: 100.0,
            });
            if let Some(root) = AppPaths::default_root() {
                if let Err(e) = apply_options(&AppPaths::new(root), &opts, detected.path()) {
                    tracing::warn!(error = %e, "初期設定を書き込めませんでした");
                }
            }
            if opts.desktop_shortcut {
                if let Err(e) = crate::packaging::create_desktop_shortcut() {
                    tracing::warn!(error = %e, "ショートカットを作れませんでした");
                }
            }
            set(Page::Done);
        });
    }

    pub fn ui(&mut self, ui: &mut Ui) -> bool {
        let p = self.palette;
        let mut close = false;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(28, 20)).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            self.header(ui);
            match self.page() {
                Page::Welcome => {
                    ui.horizontal(|ui| {
                        widgets::logo(ui, &p, 40.0);
                        ui.vertical(|ui| {
                            ui.label(widgets::title("Clipshare Quick へようこそ", &p));
                            ui.label(widgets::muted(&format!("バージョン {}", cq_core::APP_VERSION), &p));
                        });
                    });
                    ui.add_space(8.0);
                    ui.label(widgets::body(
                        "VRChatで撮った写真を自動でアップロードし、共有URLをクリップボードに入れるツールです。エクスプローラーの右クリックからも画像・動画を共有できます。",
                        &p,
                    ));
                    widgets::bottom_bar(ui, &p, |ui| {
                        if widgets::primary(ui, &p, "次へ").clicked() {
                            self.set_page(Page::License);
                        }
                        if widgets::secondary(ui, &p, "キャンセル").clicked() {
                            close = true;
                        }
                    });
                }
                Page::License => {
                    ui.label(widgets::title("使用許諾", &p));
                    widgets::card(ui, &p, |ui| {
                        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                            ui.label(widgets::strong("利用規約・プライバシーポリシー（[LICENSE TEXT]）", &p));
                            ui.label(widgets::body(
                                "本ソフトウェアは、アップロードした画像・動画のURLを発行するためにClipshareのサーバーへファイルを送信します。送信内容、保存期間（24時間）、削除方法は以下のとおりです。",
                                &p,
                            ));
                            ui.label(widgets::muted("…（以下、規約本文）", &p));
                        });
                    });
                    ui.checkbox(&mut self.agreed, "利用規約とプライバシーポリシーに同意します");
                    widgets::bottom_bar(ui, &p, |ui| {
                        if widgets::primary_enabled(ui, &p, "次へ", self.agreed).clicked() {
                            self.set_page(Page::Options);
                        }
                        if widgets::secondary(ui, &p, "戻る").clicked() {
                            self.set_page(Page::Welcome);
                        }
                    });
                }
                Page::Options => {
                    ui.label(widgets::title("インストールオプション", &p));
                    option_row(ui, &p, &mut self.opts.launch_at_startup, "Windows起動時に自動で起動する", "VRChatを遊ぶ前にトレイで待機します。あとから設定で変更できます。");
                    option_row(ui, &p, &mut self.opts.context_menu, "エクスプローラーの右クリックメニューに追加する", "画像・動画を右クリックして共有URLを発行できます。");
                    option_row(ui, &p, &mut self.opts.steamvr, "SteamVRに登録する", "VR内の通知とダッシュボードパネルを使えるようにします。");
                    option_row(ui, &p, &mut self.opts.desktop_shortcut, "デスクトップにショートカットを作る", "");
                    widgets::bottom_bar(ui, &p, |ui| {
                        if widgets::primary(ui, &p, "インストール").clicked() {
                            if self.signed_ok {
                                self.install(ui.ctx().clone());
                            } else {
                                self.set_page(Page::Trust);
                            }
                        }
                        if widgets::secondary(ui, &p, "戻る").clicked() {
                            self.set_page(Page::License);
                        }
                    });
                }
                Page::Trust => {
                    ui.label(widgets::title("署名について（暫定）", &p));
                    ui.label(widgets::body("現在の配布版は自己署名です。そのため、Windowsの「SmartScreen」で警告が表示されることがあります。", &p));
                    ui.label(widgets::muted(
                        "警告が出た場合は「詳細情報」→「実行」を選んでください。右クリックメニュー拡張の登録には、証明書の信頼を確認します。",
                        &p,
                    ));
                    widgets::card(ui, &p, |ui| {
                        ui.label(widgets::strong("署名情報（例）", &p));
                        let (pubr, fp) = match &self.signer {
                            Some(s) => (s.publisher.clone(), s.fingerprint.clone()),
                            None => ("[PUBLISHER]".to_owned(), "[FINGERPRINT]".to_owned()),
                        };
                        ui.horizontal(|ui| {
                            ui.label(widgets::muted("発行者", &p));
                            ui.label(widgets::body(&pubr, &p));
                        });
                        ui.horizontal(|ui| {
                            ui.label(widgets::muted("拇印", &p));
                            ui.label(RichText::new(fp).font(regular(12.0)).color(p.fg));
                        });
                    });
                    ui.checkbox(&mut self.trust, "この署名を信頼して右クリックメニューを登録する");
                    widgets::bottom_bar(ui, &p, |ui| {
                        if widgets::primary_enabled(ui, &p, "次へ", self.trust).clicked() {
                            self.install(ui.ctx().clone());
                        }
                        if widgets::secondary(ui, &p, "戻る").clicked() {
                            self.set_page(Page::Options);
                        }
                    });
                }
                Page::Installing { step, percent } => {
                    ui.label(widgets::title("インストールしています…", &p));
                    ui.horizontal(|ui| {
                        ui.label(widgets::body(INSTALL_STEPS.get(step).copied().unwrap_or_default(), &p));
                        ui.label(widgets::muted(&format!("{}%", percent.round() as u32), &p));
                    });
                    widgets::progress_bar(ui, &p, percent / 100.0, ui.available_width());
                    for (i, s) in INSTALL_STEPS.iter().enumerate().skip(1) {
                        let mark = if i < step { "✓" } else if i == step { "…" } else { "・" };
                        let skipped = (i == 2 && !self.opts.context_menu) || (i == 4 && !self.opts.steamvr);
                        if !skipped {
                            ui.label(RichText::new(format!("{mark} {s}")).font(regular(13.0)).color(if i <= step { p.fg } else { p.mfg }));
                        }
                    }
                }
                Page::Done => {
                    ui.label(widgets::title("インストールが完了しました", &p));
                    ui.label(widgets::muted(&format!("バージョン {}", cq_core::APP_VERSION), &p));
                    ui.label(widgets::body("このあと初回設定（写真フォルダの確認とClipshareへのログイン）に進みます。", &p));
                    ui.checkbox(&mut self.launch_after, "Clipshare Quick を起動して初回設定を開始する");
                    if self.opts.context_menu {
                        widgets::tinted(ui, p.mut_, p.bd, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(widgets::muted("右クリックメニューに表示されない場合は、エクスプローラーの再起動が必要です。", &p));
                                if widgets::small(ui, &p, "再起動").clicked() {
                                    crate::packaging::restart_explorer();
                                }
                            });
                        });
                    }
                    widgets::bottom_bar(ui, &p, |ui| {
                        if widgets::primary(ui, &p, "完了").clicked() {
                            if self.launch_after {
                                crate::packaging::launch_installed_app();
                            }
                            close = true;
                        }
                        if widgets::secondary(ui, &p, "更新履歴を開く").clicked() {
                            crate::platform::open_url(&format!("{}/qick/download", cq_core::settings::DEFAULT_API_BASE_URL));
                        }
                    });
                }
                Page::Failed { step, code } => {
                    ui.label(widgets::title("インストールできませんでした", &p));
                    widgets::badge(ui, &p, &format!("手順: {}", INSTALL_STEPS.get(step).copied().unwrap_or_default()), Tone::Danger);
                    ui.label(RichText::new(code).font(bold(13.0)).color(p.det));
                    ui.label(widgets::strong("対処", &p));
                    for l in [
                        "1. 開いているエクスプローラーを閉じてやり直す",
                        "2. 「右クリックメニューに追加する」をオフにして続ける",
                        "3. ログを開いて内容を確認する",
                    ] {
                        ui.label(RichText::new(l).font(regular(13.0)).color(p.fg));
                    }
                    widgets::bottom_bar(ui, &p, |ui| {
                        if widgets::primary(ui, &p, "再試行").clicked() {
                            self.install(ui.ctx().clone());
                        }
                        if widgets::secondary(ui, &p, "オプションを変えて再試行").clicked() {
                            self.set_page(Page::Options);
                        }
                        if widgets::secondary(ui, &p, "ログを開く").clicked() {
                            if let Some(root) = AppPaths::default_root() {
                                crate::platform::open_path(&AppPaths::new(root).log_dir());
                            }
                        }
                    });
                }
            }
        });
        close
    }
}

fn option_row(ui: &mut Ui, p: &Palette, v: &mut bool, title: &str, desc: &str) {
    ui.horizontal(|ui| {
        ui.checkbox(v, "");
        ui.vertical(|ui| {
            ui.label(widgets::strong(title, p));
            if !desc.is_empty() {
                ui.label(widgets::muted(desc, p));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_written_without_losing_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path().join("ClipshareQuick"));
        let store = SettingsStore::load(&paths.settings_file());
        store.update(|s| s.copy_url_after_upload = false).unwrap();
        let opts = SetupOptions {
            launch_at_startup: false,
            context_menu: false,
            steamvr: false,
            desktop_shortcut: false,
        };
        let folder = dir.path().join("VRChat");
        apply_options(&paths, &opts, Some(&folder)).unwrap();
        let s = SettingsStore::load(&paths.settings_file()).get();
        assert!(!s.copy_url_after_upload, "既存の設定は残す");
        assert!(!s.launch_at_startup && !s.context_menu_enabled && !s.steamvr_enabled);
        assert_eq!(s.watch_folders, vec![folder.clone()]);
        assert!(paths.shell_flag_file().exists());
        apply_options(&paths, &SetupOptions::default(), Some(&folder)).unwrap();
        assert!(!paths.shell_flag_file().exists());
        assert_eq!(
            SettingsStore::load(&paths.settings_file())
                .get()
                .watch_folders
                .len(),
            1
        );
    }

    #[test]
    fn package_is_found_next_to_setup() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_package(dir.path()).is_none());
        std::fs::write(dir.path().join("ClipshareQuick-0.1.0.msix"), b"x").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();
        assert!(
            find_package(dir.path())
                .unwrap()
                .ends_with("ClipshareQuick-0.1.0.msix")
        );
    }
}
