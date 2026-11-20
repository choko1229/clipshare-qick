//! アプリ全体で共有する状態と、UI スレッドへの命令。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use cq_core::engine::Engine;
use cq_core::logging::LogGuard;

use crate::ui::theme::Palette;

/// 開く画面。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Wizard,
    History,
    Settings(SettingsTab),
    Progress(String),
    Update,
    Uninstall,
}

/// 設定画面のタブ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Account,
    Upload,
    VrChat,
    Notifications,
    General,
    About,
}

impl SettingsTab {
    pub const ALL: [Self; 6] = [
        Self::Account,
        Self::Upload,
        Self::VrChat,
        Self::Notifications,
        Self::General,
        Self::About,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Account => "アカウント",
            Self::Upload => "アップロード",
            Self::VrChat => "VRChat",
            Self::Notifications => "通知",
            Self::General => "一般",
            Self::About => "情報",
        }
    }
}

/// UI スレッドへの命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Open(Screen),
    /// ブラウザ認可を始め、設定のアカウントを開く。
    Login,
    SetAutoUpload(bool),
    /// 履歴の URL を開く。
    OpenUrl(String),
    DeleteEntry(String),
    Quit,
}

/// ブラウザ認可の画面表示用の状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginState {
    Idle,
    Starting,
    Waiting {
        code: String,
        url: String,
        deadline: Instant,
    },
    Success {
        name: String,
    },
    Failed {
        code: String,
    },
}

/// 更新の画面表示用の状態（画面 21）。
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    Available {
        version: String,
        size_bytes: u64,
        notes: String,
    },
    Downloading {
        version: String,
        fraction: f32,
        bytes_per_sec: f64,
    },
    Ready {
        version: String,
        file: PathBuf,
    },
    Failed {
        reason: UpdateFailure,
    },
    UpToDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateFailure {
    /// 署名・ハッシュの不一致（画面 21 ⑭）。
    Signature,
    Network,
    Install,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// 共有状態。
pub struct Shared {
    pub engine: Engine,
    pub rt: tokio::runtime::Handle,
    pub palette: Palette,
    commands: std::sync::mpsc::Sender<Command>,
    main_thread: u32,
    ctx: Mutex<Option<egui::Context>>,
    login: Mutex<(LoginState, Option<tokio_util::sync::CancellationToken>)>,
    update: Mutex<UpdateState>,
    pending_update_cancel: Mutex<Option<tokio_util::sync::CancellationToken>>,
    pub log: Option<Arc<LogGuard>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

impl Shared {
    pub fn new(
        engine: Engine,
        rt: tokio::runtime::Handle,
        palette: Palette,
        commands: std::sync::mpsc::Sender<Command>,
        main_thread: u32,
        log: Option<Arc<LogGuard>>,
    ) -> Self {
        Self {
            engine,
            rt,
            palette,
            commands,
            main_thread,
            ctx: Mutex::new(None),
            login: Mutex::new((LoginState::Idle, None)),
            update: Mutex::new(UpdateState::Idle),
            pending_update_cancel: Mutex::new(None),
            log,
        }
    }

    /// UI スレッドへ命令を送る（どのスレッドからでも可）。
    pub fn send(&self, cmd: Command) {
        if self.commands.send(cmd).is_ok() {
            self.wake();
        }
    }

    /// UI を描き直させる（ウィンドウが無ければメインループを起こす）。
    pub fn wake(&self) {
        if let Some(ctx) = lock(&self.ctx).as_ref() {
            ctx.request_repaint();
        }
        crate::platform::wake_thread(self.main_thread);
    }

    pub fn set_ctx(&self, ctx: Option<egui::Context>) {
        *lock(&self.ctx) = ctx;
    }

    // ---- ログイン ----

    pub fn login_state(&self) -> LoginState {
        lock(&self.login).0.clone()
    }

    fn set_login(&self, s: LoginState) {
        lock(&self.login).0 = s;
        self.wake();
    }

    /// ブラウザ認可を始める（進行中なら何もしない）。
    pub fn start_login(self: &Arc<Self>) {
        {
            let mut g = lock(&self.login);
            if matches!(g.0, LoginState::Starting | LoginState::Waiting { .. }) {
                return;
            }
            g.0 = LoginState::Starting;
        }
        let me = Arc::clone(self);
        self.rt.spawn(async move {
            let session = match me.engine.start_login().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(code = e.code(), "ブラウザ認可を開始できませんでした");
                    me.set_login(LoginState::Failed {
                        code: e.code().to_owned(),
                    });
                    return;
                }
            };
            let url = session.browser_url().to_string();
            let timeout = std::time::Duration::from_secs(
                me.engine.settings().get().advanced.auth_timeout_secs,
            );
            {
                let mut g = lock(&me.login);
                g.0 = LoginState::Waiting {
                    code: session.connection_code().to_owned(),
                    url: url.clone(),
                    deadline: Instant::now() + timeout,
                };
                g.1 = Some(session.cancel_token());
            }
            me.wake();
            if !crate::platform::open_url(&url) {
                tracing::warn!("ブラウザを開けませんでした");
            }
            let result = session.finish().await;
            lock(&me.login).1 = None;
            match result {
                Ok(user) => me.set_login(LoginState::Success { name: user.name }),
                Err(e) => {
                    tracing::info!(code = e.code(), "ブラウザ認可が完了しませんでした");
                    let code = e.code().to_owned();
                    if code == "AUTH_CANCELLED" {
                        me.set_login(LoginState::Idle);
                    } else {
                        me.set_login(LoginState::Failed { code });
                    }
                }
            }
        });
    }

    pub fn cancel_login(&self) {
        let token = lock(&self.login).1.take();
        if let Some(t) = token {
            t.cancel();
        }
        self.set_login(LoginState::Idle);
    }

    pub fn reset_login(&self) {
        let mut g = lock(&self.login);
        if !matches!(g.0, LoginState::Starting | LoginState::Waiting { .. }) {
            g.0 = LoginState::Idle;
        }
    }

    // ---- 更新 ----

    pub fn update_state(&self) -> UpdateState {
        lock(&self.update).clone()
    }

    pub fn set_update_state(&self, s: UpdateState) {
        *lock(&self.update) = s;
        self.wake();
    }

    pub fn set_update_cancel(&self, c: Option<tokio_util::sync::CancellationToken>) {
        *lock(&self.pending_update_cancel) = c;
    }

    pub fn cancel_update_download(&self) {
        if let Some(c) = lock(&self.pending_update_cancel).take() {
            c.cancel();
        }
    }
}
