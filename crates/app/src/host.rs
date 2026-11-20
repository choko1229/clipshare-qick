//! ウィンドウの管理。
//!
//! トレイ常駐中はウィンドウを持たず、Win32 のメッセージループだけを回す（トレイのイベントもここで届く）。
//! 画面を開くときだけ `eframe::run_native`（`run_and_return`）でウィンドウを作り、閉じたら戻ってくる。
//! ウィンドウ表示中に別の画面を開く命令が来たら、いまのウィンドウを閉じて次を開く。
//! 右クリック共有の進捗は、別の画面が開いていれば子ウィンドウとして重ねて表示する。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, TranslateMessage,
};

use crate::app::{Command, Screen, Shared};
use crate::tray::Tray;
use crate::ui;

/// 画面からの要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    Open(Screen),
    Quit,
}

/// メインループ。終了するまで戻らない。
pub fn run(
    shared: Arc<Shared>,
    rx: Receiver<Command>,
    tray: Rc<RefCell<Tray>>,
    first: Option<Screen>,
) {
    let rx = Arc::new(Mutex::new(rx));
    let mut next = first;
    loop {
        if let Some(screen) = next.take() {
            match open_window(&shared, &rx, &tray, screen) {
                WindowResult::Closed => {}
                WindowResult::Next(s) => {
                    next = Some(s);
                    continue;
                }
                WindowResult::Quit => return,
            }
        }
        // ウィンドウなしで待つ
        match wait_idle(&shared, &rx, &tray) {
            Some(Command::Quit) | None => return,
            Some(Command::Open(s)) => next = Some(s),
            Some(other) => {
                if let Some(s) = handle_common(&shared, other) {
                    next = Some(s);
                }
            }
        }
    }
}

/// 画面を開かずに処理できる命令。画面を開く必要があれば返す。
fn handle_common(shared: &Arc<Shared>, cmd: Command) -> Option<Screen> {
    match cmd {
        Command::SetAutoUpload(on) => {
            if let Err(e) = crate::actions::set_auto_upload(shared, on) {
                tracing::warn!(error = %e, "自動アップロードを切り替えられませんでした");
            }
            None
        }
        Command::OpenUrl(url) => {
            crate::platform::open_url(&url);
            None
        }
        Command::DeleteEntry(id) => {
            crate::actions::delete_entry(shared, id);
            None
        }
        Command::Login => {
            shared.start_login();
            Some(Screen::Settings(crate::app::SettingsTab::Account))
        }
        Command::Open(s) => Some(s),
        Command::Quit => None,
    }
}

/// Win32 メッセージを処理しながら命令を待つ。
fn wait_idle(
    shared: &Arc<Shared>,
    rx: &Arc<Mutex<Receiver<Command>>>,
    tray: &Rc<RefCell<Tray>>,
) -> Option<Command> {
    loop {
        if let Ok(mut t) = tray.try_borrow_mut() {
            t.refresh(shared);
        }
        if let Ok(g) = rx.lock() {
            match g.try_recv() {
                Ok(c) => return Some(c),
                Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let mut msg = MSG::default();
        // SAFETY: 標準的なメッセージループ。msg はローカル変数。
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            return Some(Command::Quit);
        }
        // SAFETY: GetMessageW が埋めたメッセージをそのまま渡す。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

enum WindowResult {
    Closed,
    Next(Screen),
    Quit,
}

/// 画面ごとの状態。
enum ScreenState {
    Wizard(ui::wizard::Wizard),
    History(ui::history::HistoryPanel),
    Settings(ui::settings::SettingsWindow),
    Progress(ui::progress::ProgressWindow),
    Update(ui::update::UpdateWindow),
    Uninstall(ui::uninstall::UninstallWindow),
}

impl ScreenState {
    fn new(shared: &Arc<Shared>, s: &Screen) -> Self {
        match s {
            Screen::Wizard => Self::Wizard(ui::wizard::Wizard::new(shared)),
            Screen::History => Self::History(ui::history::HistoryPanel::new(shared)),
            Screen::Settings(tab) => Self::Settings(ui::settings::SettingsWindow::new(*tab)),
            Screen::Progress(id) => Self::Progress(ui::progress::ProgressWindow::new(id.clone())),
            Screen::Update => Self::Update(ui::update::UpdateWindow),
            Screen::Uninstall => Self::Uninstall(ui::uninstall::UninstallWindow::default()),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) -> Action {
        match self {
            Self::Wizard(s) => s.ui(ui, shared),
            Self::History(s) => s.ui(ui, shared),
            Self::Settings(s) => s.ui(ui, shared),
            Self::Progress(s) => s.ui(ui, shared),
            Self::Update(s) => s.ui(ui, shared),
            Self::Uninstall(s) => s.ui(ui, shared),
        }
    }
}

/// 画面ごとのウィンドウ設定。
fn viewport_for(screen: &Screen) -> egui::ViewportBuilder {
    let icon = Arc::new(crate::tray::app_icon());
    let base = egui::ViewportBuilder::default()
        .with_icon(icon)
        .with_resizable(false);
    match screen {
        Screen::History => {
            let (x, y) = crate::tray::panel_position(400.0, 460.0);
            base.with_inner_size([400.0, 460.0])
                .with_decorations(false)
                .with_always_on_top()
                .with_taskbar(false)
                .with_position([x, y])
                .with_title("Clipshare Quick")
        }
        Screen::Wizard => base
            .with_inner_size([560.0, 460.0])
            .with_title("Clipshare Quick — はじめに"),
        Screen::Settings(_) => base
            .with_inner_size([760.0, 540.0])
            .with_title("Clipshare Quick — 設定"),
        Screen::Progress(_) => base
            .with_inner_size([420.0, 300.0])
            .with_title("Clipshare Quick"),
        Screen::Update => base
            .with_inner_size([560.0, 380.0])
            .with_title("Clipshare Quick アップデート"),
        Screen::Uninstall => base
            .with_inner_size([560.0, 380.0])
            .with_title("Clipshare Quick アンインストール"),
    }
}

struct WindowApp {
    shared: Arc<Shared>,
    tray: Rc<RefCell<Tray>>,
    rx: Arc<Mutex<Receiver<Command>>>,
    screen: Screen,
    state: ScreenState,
    /// 重ねて表示する進捗ウィンドウ。
    overlay: Option<ui::progress::ProgressWindow>,
    result: Arc<Mutex<Option<WindowResult>>>,
    was_focused: bool,
}

impl WindowApp {
    fn finish(&mut self, ctx: &egui::Context, r: WindowResult) {
        if let Ok(mut g) = self.result.lock() {
            if g.is_none() {
                *g = Some(r);
            }
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn apply(&mut self, ctx: &egui::Context, a: Action) {
        match a {
            Action::None => {}
            Action::Close => self.finish(ctx, WindowResult::Closed),
            Action::Quit => self.finish(ctx, WindowResult::Quit),
            Action::Open(s) if s == self.screen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus)
            }
            Action::Open(Screen::Progress(id)) => {
                self.overlay = Some(ui::progress::ProgressWindow::new(id));
            }
            Action::Open(s) => self.finish(ctx, WindowResult::Next(s)),
        }
    }

    fn poll_commands(&mut self, ctx: &egui::Context) {
        loop {
            let cmd = match self.rx.lock() {
                Ok(g) => g.try_recv(),
                Err(_) => return,
            };
            match cmd {
                Ok(Command::Quit) => return self.finish(ctx, WindowResult::Quit),
                Ok(Command::Open(s)) => self.apply(ctx, Action::Open(s)),
                Ok(other) => {
                    if let Some(s) = handle_common(&self.shared, other) {
                        self.apply(ctx, Action::Open(s));
                    }
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => return self.finish(ctx, WindowResult::Quit),
            }
        }
    }
}

impl eframe::App for WindowApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_commands(ctx);
        if let Ok(mut t) = self.tray.try_borrow_mut() {
            t.refresh(&self.shared);
        }
        let p = self.shared.palette;
        let frame = egui::Frame::new()
            .fill(p.bg)
            .inner_margin(egui::Margin::ZERO);
        let mut action = Action::None;
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            action = self.state.ui(ui, &self.shared);
        });
        self.apply(ctx, action);

        if let Some(ov) = self.overlay.as_mut() {
            let mut close = false;
            let shared = Arc::clone(&self.shared);
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("progress"),
                egui::ViewportBuilder::default()
                    .with_inner_size([420.0, 300.0])
                    .with_resizable(false)
                    .with_title("Clipshare Quick"),
                |ctx, _class| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(p.bg))
                        .show(ctx, |ui| {
                            if ov.ui(ui, &shared) != Action::None {
                                close = true;
                            }
                        });
                    if ctx.input(|i| i.viewport().close_requested()) {
                        close = true;
                    }
                },
            );
            if close {
                self.overlay = None;
            }
        }

        // 履歴パネルはフォーカスを失ったら閉じる。
        if self.screen == Screen::History {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
            if focused {
                self.was_focused = true;
            } else if self.was_focused && self.overlay.is_none() {
                self.finish(ctx, WindowResult::Closed);
            }
        }
        // 時間表示・進捗の更新のため定期的に描き直す。
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

fn open_window(
    shared: &Arc<Shared>,
    rx: &Arc<Mutex<Receiver<Command>>>,
    tray: &Rc<RefCell<Tray>>,
    screen: Screen,
) -> WindowResult {
    tracing::debug!(?screen, "画面を開きます");
    if screen == Screen::History {
        shared.engine.acknowledge_errors();
    }
    let result = Arc::new(Mutex::new(None));
    let options = eframe::NativeOptions {
        viewport: viewport_for(&screen),
        centered: screen != Screen::History,
        run_and_return: true,
        ..Default::default()
    };
    let app = WindowApp {
        shared: Arc::clone(shared),
        tray: Rc::clone(tray),
        rx: Arc::clone(rx),
        state: ScreenState::new(shared, &screen),
        screen: screen.clone(),
        overlay: None,
        result: Arc::clone(&result),
        was_focused: false,
    };
    let shared2 = Arc::clone(shared);
    let r = eframe::run_native(
        "Clipshare Quick",
        options,
        Box::new(move |cc| {
            // フォントは次のフレームから有効になるため、最初の描画より前（作成時）に設定する。
            ui::theme::apply(&cc.egui_ctx, &shared2.palette);
            shared2.set_ctx(Some(cc.egui_ctx.clone()));
            Ok(Box::new(app))
        }),
    );
    shared.set_ctx(None);
    if let Ok(mut t) = tray.try_borrow_mut() {
        t.refresh(shared);
    }
    if let Err(e) = r {
        tracing::error!(error = %e, "ウィンドウを開けませんでした");
    }
    match result.lock().ok().and_then(|mut g| g.take()) {
        Some(r) => r,
        None => WindowResult::Closed,
    }
}
