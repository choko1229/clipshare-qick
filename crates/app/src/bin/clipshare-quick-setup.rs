//! Clipshare Quick のセットアップ（インストーラー画面 02・03）。
//! 同じフォルダの `.msix` をインストールし、オプションを初期設定として書き込む。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use clipshare_quick::setup::SetupApp;

struct App(SetupApp);

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let bg = self.0.palette().bg;
        let mut close = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(bg))
            .show(ctx, |ui| close = self.0.ui(ui));
        if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default();
    let setup = SetupApp::new(&dir);
    let palette = setup.palette();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 380.0])
            .with_resizable(false)
            .with_title("Clipshare Quick セットアップ")
            .with_icon(Arc::new(clipshare_quick::tray::app_icon())),
        centered: true,
        ..Default::default()
    };
    let _ = eframe::run_native(
        "Clipshare Quick セットアップ",
        options,
        Box::new(move |cc| {
            clipshare_quick::ui::theme::apply(&cc.egui_ctx, &palette);
            Ok(Box::new(App(setup)))
        }),
    );
}
