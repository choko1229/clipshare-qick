//! アプリの中核。フォルダ監視・送信キュー・履歴・アカウント・状態を束ね、UI へスナップショットと
//! イベントを配る。UI からの操作（再試行・削除・ログインなど）もここを通す。
//!
//! 主な規則（SPEC / INSTRUCTIONS §3.2）:
//! - 自動アップロードは「同意済みかつ ON」のときだけ。手動の共有（右クリック）は利用者の明示的な操作として送る。
//! - 1日上限（429 `DAILY_LIMIT_EXCEEDED`）: 自動アップロードを止め、リセット時刻（無ければ現地の日付変化）で再開。
//!   停止中に撮った写真は送らない（キューにも履歴にも入れない）。
//! - 401: 再ログインが必要な状態にする。**匿名へ自動で切り替えない。**
//! - 接続エラー: 「接続待ち」にして定期的に接続を確認し、戻ったら自動で送る。
//! - 5xx・タイムアウト: 再試行してもダメなら失敗として履歴に残す（手動で再試行できる）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::api::{ApiClient, ApiError, DeleteAuth, Limits, User};
use crate::auth::{AuthError, PendingAuth};
use crate::clipboard::{Clipboard, join_urls};
use crate::history::{
    EntrySource, EntryStatus, Failure, Fingerprint, HistoryEntry, HistoryStore, ProcessedSet,
    new_id,
};
use crate::media::MediaKind;
use crate::pathcheck::validate_share_path;
use crate::paths::AppPaths;
use crate::secret::SecretString;
use crate::settings::{Quality, Settings, SettingsStore, SettingsUpdateError};
use crate::state::{Account, DailyPause, StateStore};
use crate::token_store::TokenStore;
use crate::upload::{UploadError, UploadJob, upload_file};
use crate::watcher::{FolderStatus, FolderWatcher, ReadyFile, WatchOptions};

/// トレイアイコンの状態（画面 08）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Uploading,
    Error,
    Paused,
    NeedsLogin,
    Offline,
}

impl TrayState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "待機中",
            Self::Uploading => "アップロード中",
            Self::Error => "エラーあり",
            Self::Paused => "一時停止",
            Self::NeedsLogin => "要ログイン",
            Self::Offline => "オフライン",
        }
    }
}

/// アカウントの表示状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountView {
    Anonymous,
    LoggedIn {
        name: String,
    },
    /// 連携していたがトークンが無効（再ログインが必要）。
    NeedsRelogin {
        name: String,
    },
}

/// UI 向けの状態のまとめ。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub tray: TrayState,
    pub auto_upload: bool,
    pub account: AccountView,
    pub limits: Option<Limits>,
    pub offline: bool,
    pub waiting_count: usize,
    pub daily_paused: bool,
    pub history: Vec<HistoryEntry>,
    pub folders: Vec<FolderStatus>,
    pub unseen_error: bool,
    /// 403（BAN など）で利用できない状態。
    pub blocked: bool,
}

/// UI・通知・VR 向けのイベント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    UploadStarted {
        entry_id: String,
        file_name: String,
        source: EntrySource,
    },
    Uploaded {
        entry_id: String,
        file_name: String,
        url: String,
        source: EntrySource,
        processing: bool,
        /// クリップボードにコピーしたか。
        copied: bool,
        /// 連続撮影でまとめてコピーしている URL の数（1 なら単独）。
        copied_count: usize,
        kind: MediaKind,
    },
    Shrunk {
        file_name: String,
        original_bytes: u64,
    },
    VideoTooLarge {
        file_name: String,
        size_bytes: u64,
    },
    Failed {
        entry_id: String,
        file_name: String,
        /// 失敗の種類（API のエラーコードなど）。
        code: String,
        message: String,
    },
    DailyLimitReached {
        anonymous: bool,
        limit: Option<u64>,
    },
    AuthExpired,
    /// 403 FORBIDDEN を受けた（BAN など。再ログインでは解決しない）。
    AccountBlocked,
    Offline {
        waiting: usize,
    },
    BackOnline,
    AutoUploadChanged {
        enabled: bool,
    },
    BatchStarted {
        batch_id: String,
    },
    BatchFinished {
        batch_id: String,
        succeeded: usize,
        failed: usize,
        copied: bool,
    },
    LoggedIn {
        name: String,
    },
    LoggedOut,
}

/// 右クリック共有の 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchItem {
    pub file_name: String,
    pub entry_id: Option<String>,
    /// 受け付けなかった理由（パス検証の失敗など）。
    pub rejected: Option<String>,
}

#[derive(Debug)]
struct Batch {
    items: Vec<BatchItem>,
    cancel: CancellationToken,
    finished: bool,
}

/// 右クリック共有の進捗（画面 15）。
#[derive(Debug, Clone)]
pub struct BatchView {
    pub items: Vec<BatchItemView>,
    pub finished: bool,
    pub urls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BatchItemView {
    pub file_name: String,
    pub status: BatchItemStatus,
    /// 0.0〜1.0
    pub progress: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchItemStatus {
    Waiting,
    Uploading,
    Done,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("履歴が見つかりません")]
    EntryNotFound,
    #[error("この項目は操作できません")]
    InvalidState,
    #[error("連携が切れています。再ログインしてください")]
    NeedsRelogin,
    #[error("クリップボードにコピーできませんでした")]
    Clipboard,
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Settings(#[from] SettingsUpdateError),
    #[error("保存に失敗しました")]
    Storage(#[from] crate::error::StorageError),
    #[error("資格情報マネージャーを操作できませんでした")]
    TokenStore(#[from] crate::token_store::TokenStoreError),
}

/// エンジンの依存。
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub paths: AppPaths,
    pub settings: SettingsStore,
    pub token_store: Arc<dyn TokenStore>,
    pub clipboard: Arc<dyn Clipboard>,
}

#[derive(Debug)]
struct Job {
    entry_id: String,
    /// 利用者の明示的な操作（右クリック・手動の再試行）。1日上限の停止中でも送る。
    manual: bool,
}

#[derive(Debug, Default)]
struct ClipBatch {
    urls: Vec<String>,
    last: Option<Instant>,
}

struct Inner {
    paths: AppPaths,
    settings: SettingsStore,
    token_store: Arc<dyn TokenStore>,
    clipboard: Arc<dyn Clipboard>,
    api: RwLock<ApiClient>,
    history: Mutex<HistoryStore>,
    processed: Mutex<ProcessedSet>,
    state: Mutex<StateStore>,
    limits: Mutex<Option<Limits>>,
    online: AtomicBool,
    probing: AtomicBool,
    unseen_error: AtomicBool,
    queue: mpsc::UnboundedSender<Job>,
    events: broadcast::Sender<EngineEvent>,
    snapshot: watch::Sender<Snapshot>,
    progress: Mutex<HashMap<String, (u64, u64)>>,
    current: Mutex<Option<(String, CancellationToken)>>,
    clip_batch: Mutex<ClipBatch>,
    batches: Mutex<HashMap<String, Batch>>,
    watcher: Mutex<Option<FolderWatcher>>,
    /// 監視しているフォルダと、その監視を始めた時刻（設定変更で監視し直しても、同じフォルダなら引き継ぐ）。
    watch_started: Mutex<Option<(Vec<PathBuf>, SystemTime)>>,
    folders: Mutex<Vec<FolderStatus>>,
    shutdown: CancellationToken,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

/// エンジン。複製は安価。
#[derive(Debug, Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn build_api(s: &Settings) -> Result<ApiClient, ApiError> {
    ApiClient::new(
        &s.advanced.api_base_url,
        Duration::from_secs(s.advanced.request_timeout_secs),
        Duration::from_secs(s.advanced.upload_timeout_secs),
    )
}

impl Engine {
    /// 起動する（tokio ランタイム内で呼ぶ）。前回の送信待ちは送り直す。
    pub fn start(cfg: EngineConfig) -> Result<Self, ApiError> {
        let s = cfg.settings.get();
        let api = build_api(&s)?;
        let history = HistoryStore::load(&cfg.paths.history_file(), s.advanced.history_limit);
        let processed = ProcessedSet::load(&cfg.paths.processed_file(), ProcessedSet::DEFAULT_CAP);
        let state = StateStore::load(&cfg.paths.state_file());
        let (queue, queue_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(256);
        let (snapshot, _) = watch::channel(Snapshot {
            tray: TrayState::Idle,
            auto_upload: s.auto_upload_active(),
            account: AccountView::Anonymous,
            limits: None,
            offline: false,
            waiting_count: 0,
            daily_paused: false,
            history: Vec::new(),
            folders: Vec::new(),
            unseen_error: false,
            blocked: false,
        });
        let inner = Arc::new(Inner {
            paths: cfg.paths,
            settings: cfg.settings,
            token_store: cfg.token_store,
            clipboard: cfg.clipboard,
            api: RwLock::new(api),
            history: Mutex::new(history),
            processed: Mutex::new(processed),
            state: Mutex::new(state),
            limits: Mutex::new(None),
            online: AtomicBool::new(true),
            probing: AtomicBool::new(false),
            unseen_error: AtomicBool::new(false),
            queue,
            events,
            snapshot,
            progress: Mutex::new(HashMap::new()),
            current: Mutex::new(None),
            clip_batch: Mutex::new(ClipBatch::default()),
            batches: Mutex::new(HashMap::new()),
            watcher: Mutex::new(None),
            watch_started: Mutex::new(None),
            folders: Mutex::new(Vec::new()),
            shutdown: CancellationToken::new(),
        });
        let engine = Self { inner };
        engine.expire_pause();
        engine.recover_orphaned_token();
        let pending = lock(&engine.inner.history).pending_ids();
        for id in pending {
            engine.enqueue(id, false);
        }
        tokio::spawn(engine.clone().worker(queue_rx));
        tokio::spawn(engine.clone().settings_listener());
        tokio::spawn(engine.clone().pause_timer());
        engine.restart_watcher();
        let e = engine.clone();
        tokio::spawn(async move { e.refresh_limits().await });
        engine.publish();
        tracing::info!(version = crate::APP_VERSION, "エンジンを開始しました");
        Ok(engine)
    }

    /// 停止する（キャンセルと監視の停止）。
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
        if let Some((_, c)) = lock(&self.inner.current).as_ref() {
            c.cancel();
        }
        *lock(&self.inner.watcher) = None;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.inner.events.subscribe()
    }

    pub fn snapshot(&self) -> watch::Receiver<Snapshot> {
        self.inner.snapshot.subscribe()
    }

    pub fn settings(&self) -> &SettingsStore {
        &self.inner.settings
    }

    pub fn paths(&self) -> &AppPaths {
        &self.inner.paths
    }

    pub fn api(&self) -> ApiClient {
        match self.inner.api.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    /// 送信中の進捗（0.0〜1.0）。
    pub fn progress_of(&self, entry_id: &str) -> Option<f32> {
        lock(&self.inner.progress)
            .get(entry_id)
            .map(|&(done, total)| {
                if total == 0 {
                    0.0
                } else {
                    (done as f64 / total as f64).clamp(0.0, 1.0) as f32
                }
            })
    }

    fn emit(&self, ev: EngineEvent) {
        let _ = self.inner.events.send(ev);
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    /// スナップショットを作り直して配る。
    fn publish(&self) {
        let now = Self::now();
        let settings = self.inner.settings.get();
        let (history, waiting) = {
            let h = lock(&self.inner.history);
            let waiting = h
                .entries()
                .iter()
                .filter(|e| matches!(e.status, EntryStatus::Waiting | EntryStatus::Queued))
                .count();
            (h.entries().to_vec(), waiting)
        };
        let (account, paused, blocked) = {
            let st = lock(&self.inner.state);
            let account = match (&st.state.account, st.state.needs_relogin) {
                (None, _) => AccountView::Anonymous,
                (Some(a), false) => AccountView::LoggedIn {
                    name: a.user.name.clone(),
                },
                (Some(a), true) => AccountView::NeedsRelogin {
                    name: a.user.name.clone(),
                },
            };
            (account, st.paused(now), st.state.account_blocked)
        };
        let offline = !self.inner.online.load(Ordering::SeqCst);
        let uploading = history.iter().any(|e| e.status == EntryStatus::Uploading);
        let unseen_error = self.inner.unseen_error.load(Ordering::SeqCst);
        let tray = if matches!(account, AccountView::NeedsRelogin { .. }) {
            TrayState::NeedsLogin
        } else if uploading {
            TrayState::Uploading
        } else if offline {
            TrayState::Offline
        } else if unseen_error || blocked {
            TrayState::Error
        } else if !settings.auto_upload_active() || paused {
            TrayState::Paused
        } else {
            TrayState::Idle
        };
        let snap = Snapshot {
            tray,
            auto_upload: settings.auto_upload_active(),
            account,
            limits: lock(&self.inner.limits).clone(),
            offline,
            waiting_count: waiting,
            daily_paused: paused,
            history,
            folders: lock(&self.inner.folders).clone(),
            unseen_error,
            blocked,
        };
        self.inner.snapshot.send_replace(snap);
    }

    /// エラー表示を確認済みにする（履歴パネルを開いたとき）。
    pub fn acknowledge_errors(&self) {
        if self.inner.unseen_error.swap(false, Ordering::SeqCst) {
            self.publish();
        }
    }

    fn save_history(&self, h: &HistoryStore) {
        if let Err(e) = h.save() {
            tracing::error!(error = %e, "履歴を保存できませんでした");
        }
    }

    fn save_state(&self, st: &StateStore) {
        if let Err(e) = st.save() {
            tracing::error!(error = %e, "実行状態を保存できませんでした");
        }
    }

    fn enqueue(&self, entry_id: String, manual: bool) {
        if self.inner.queue.send(Job { entry_id, manual }).is_err() {
            tracing::warn!("送信キューが閉じています");
        }
    }

    // ---- アカウント ----

    /// 送信に使うトークン。匿名利用なら `Ok(None)`。連携中なのに取れなければ再ログインが必要。
    fn token(&self) -> Result<Option<SecretString>, EngineError> {
        let (has_account, needs_relogin) = {
            let st = lock(&self.inner.state);
            (st.state.account.is_some(), st.state.needs_relogin)
        };
        if !has_account {
            // 実行状態のファイルが失われても、資格情報マネージャーにトークンがあれば匿名にはしない。
            return match self.inner.token_store.load() {
                Ok(t) => Ok(t),
                Err(e) => {
                    tracing::warn!(error = %e, "トークンを読み込めません");
                    Err(EngineError::NeedsRelogin)
                }
            };
        }
        if needs_relogin {
            return Err(EngineError::NeedsRelogin);
        }
        match self.inner.token_store.load() {
            Ok(Some(t)) => Ok(Some(t)),
            Ok(None) => {
                tracing::warn!("連携中ですがトークンが見つかりません");
                self.mark_needs_relogin();
                Err(EngineError::NeedsRelogin)
            }
            Err(e) => {
                tracing::warn!(error = %e, "トークンを読み込めません");
                self.mark_needs_relogin();
                Err(EngineError::NeedsRelogin)
            }
        }
    }

    /// 実行状態にアカウントが無いのにトークンが残っている場合、`/me` でアカウントを復元する。
    fn recover_orphaned_token(&self) {
        let has_account = lock(&self.inner.state).state.account.is_some();
        if has_account || !matches!(self.inner.token_store.load(), Ok(Some(_))) {
            return;
        }
        tracing::warn!("連携の記録が無いためトークンからアカウントを復元します");
        let e = self.clone();
        tokio::spawn(async move {
            let Ok(Some(token)) = e.inner.token_store.load() else {
                return;
            };
            match e.api().me(Some(&token)).await {
                Ok(me) if me.authenticated => {
                    if let Some(user) = me.user {
                        let mut st = lock(&e.inner.state);
                        st.state.account = Some(Account {
                            user,
                            token_id: String::new(),
                            linked_at: Self::now(),
                        });
                        st.state.needs_relogin = false;
                        e.save_state(&st);
                    }
                    *lock(&e.inner.limits) = Some(me.limits);
                    e.publish();
                }
                Ok(_) | Err(ApiError::Unauthorized) => e.mark_needs_relogin(),
                Err(err) => tracing::warn!(code = err.code(), "アカウントを復元できませんでした"),
            }
        });
    }

    /// 403 による利用停止を記録・解除する。停止中は自動アップロードを送らない（匿名にも切り替えない）。
    fn set_blocked(&self, blocked: bool) {
        let changed = {
            let mut st = lock(&self.inner.state);
            if st.state.account_blocked == blocked {
                false
            } else {
                st.state.account_blocked = blocked;
                self.save_state(&st);
                true
            }
        };
        if changed {
            if blocked {
                tracing::warn!(
                    "403 を受けたため自動アップロードを停止します（アカウントの状態を確認する必要があります）"
                );
                self.emit(EngineEvent::AccountBlocked);
            } else {
                tracing::info!("利用停止が解除されました");
            }
            self.publish();
        }
    }

    pub fn is_blocked(&self) -> bool {
        lock(&self.inner.state).state.account_blocked
    }

    fn mark_needs_relogin(&self) {
        let changed = {
            let mut st = lock(&self.inner.state);
            if st.state.account.is_none() && matches!(self.inner.token_store.load(), Ok(Some(_))) {
                st.state.account = Some(Account {
                    user: User {
                        id: String::new(),
                        name: "（不明）".to_owned(),
                    },
                    token_id: String::new(),
                    linked_at: Self::now(),
                });
            }
            if st.state.account.is_some() && !st.state.needs_relogin {
                st.state.needs_relogin = true;
                self.save_state(&st);
                true
            } else {
                false
            }
        };
        if changed {
            tracing::warn!("連携が無効になりました（再ログインが必要）");
            self.emit(EngineEvent::AuthExpired);
            self.publish();
        }
    }

    /// 上限情報を取り直す。
    pub async fn refresh_limits(&self) {
        let token = match self.token() {
            Ok(t) => t,
            Err(_) => return,
        };
        match self.api().me(token.as_ref()).await {
            Ok(me) => {
                if token.is_some() && !me.authenticated {
                    self.mark_needs_relogin();
                }
                self.set_blocked(false);
                *lock(&self.inner.limits) = Some(me.limits);
                self.set_online(true);
                self.publish();
            }
            Err(ApiError::Unauthorized) if token.is_some() => self.mark_needs_relogin(),
            Err(ApiError::Forbidden) => self.set_blocked(true),
            Err(e) if e.retry_class() == crate::api::RetryClass::WaitForConnection => {
                self.set_offline()
            }
            Err(e) => tracing::warn!(code = e.code(), "上限情報を取得できませんでした"),
        }
    }

    /// ブラウザ認可を始める。
    pub async fn start_login(&self) -> Result<LoginSession, AuthError> {
        let base = self.api().base_url().clone();
        let pending = PendingAuth::start_with_platform(
            &base,
            &crate::auth::device_name(),
            Some(&crate::auth::device_platform()),
        )
        .await?;
        Ok(LoginSession {
            engine: self.clone(),
            pending,
            cancel: CancellationToken::new(),
        })
    }

    async fn complete_login(&self, resp: crate::api::TokenResponse) -> Result<User, AuthError> {
        self.inner
            .token_store
            .save(&resp.token)
            .map_err(|e| AuthError::Exchange(ApiError::Io(std::io::Error::other(e.to_string()))))?;
        {
            let mut st = lock(&self.inner.state);
            st.state.account = Some(Account {
                user: resp.user.clone(),
                token_id: resp.token_id.clone(),
                linked_at: Self::now(),
            });
            st.state.needs_relogin = false;
            st.state.account_blocked = false;
            if let Err(e) = st.save() {
                // 連携の記録を残せないと、次回起動時に状態が食い違うため失敗として扱う。
                tracing::error!(error = %e, "連携の記録を保存できませんでした");
                return Err(AuthError::Exchange(ApiError::Io(std::io::Error::other(
                    e.to_string(),
                ))));
            }
        }
        *lock(&self.inner.limits) = Some(resp.limits);
        tracing::info!("Clipshare と連携しました");
        self.emit(EngineEvent::LoggedIn {
            name: resp.user.name.clone(),
        });
        self.publish();
        Ok(resp.user)
    }

    /// 連携を解除する。サーバー側の失効に成功したら `true`（オフラインなどで失敗しても、ローカルの
    /// トークンは必ず削除する）。
    pub async fn logout(&self) -> Result<bool, EngineError> {
        let token = match self.inner.token_store.load() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "トークンを読み込めないため、サーバー側の解除はできません");
                self.inner.token_store.delete()?;
                let mut st = lock(&self.inner.state);
                st.state.account = None;
                st.state.needs_relogin = false;
                self.save_state(&st);
                drop(st);
                self.emit(EngineEvent::LoggedOut);
                self.publish();
                return Ok(false);
            }
        };
        let server_revoked = match &token {
            Some(t) => match self.api().revoke_token(t).await {
                Ok(()) | Err(ApiError::Unauthorized) => true,
                Err(e) => {
                    tracing::warn!(code = e.code(), "サーバー側の連携解除に失敗しました");
                    false
                }
            },
            None => true,
        };
        self.inner.token_store.delete()?;
        {
            let mut st = lock(&self.inner.state);
            st.state.account = None;
            st.state.needs_relogin = false;
            st.state.account_blocked = false;
            self.save_state(&st);
        }
        *lock(&self.inner.limits) = None;
        tracing::info!(server_revoked, "連携を解除しました");
        self.emit(EngineEvent::LoggedOut);
        self.publish();
        let e = self.clone();
        tokio::spawn(async move { e.refresh_limits().await });
        Ok(server_revoked)
    }

    pub fn account(&self) -> Option<Account> {
        lock(&self.inner.state).state.account.clone()
    }

    /// 前回の更新確認の時刻。
    pub fn last_update_check(&self) -> Option<DateTime<Utc>> {
        lock(&self.inner.state).state.last_update_check
    }

    pub fn set_last_update_check(&self, t: DateTime<Utc>) {
        let mut st = lock(&self.inner.state);
        st.state.last_update_check = Some(t);
        self.save_state(&st);
    }

    // ---- 設定の操作 ----

    /// 自動アップロードを切り替える。利用者が明示的に ON にした場合は、送信への同意として記録する。
    pub fn set_auto_upload(&self, enabled: bool) -> Result<(), EngineError> {
        let before = self.inner.settings.get().auto_upload_active();
        self.inner.settings.update(|s| {
            s.auto_upload = enabled;
            if enabled {
                s.auto_upload_consented = true;
            }
        })?;
        if before != enabled {
            tracing::info!(enabled, "自動アップロードを切り替えました");
            self.emit(EngineEvent::AutoUploadChanged { enabled });
        }
        self.publish();
        Ok(())
    }

    pub fn set_default_quality(&self, q: Quality) -> Result<(), EngineError> {
        self.inner.settings.update(|s| s.default_quality = q)?;
        Ok(())
    }

    async fn settings_listener(self) {
        let mut rx = self.inner.settings.subscribe();
        let mut prev = rx.borrow().clone();
        loop {
            tokio::select! {
                () = self.inner.shutdown.cancelled() => return,
                r = rx.changed() => if r.is_err() { return },
            }
            let next = rx.borrow_and_update().clone();
            if next.watch_folders != prev.watch_folders
                || next.watch_videos != prev.watch_videos
                || next.advanced.stability != prev.advanced.stability
            {
                self.restart_watcher();
            }
            if next.advanced.api_base_url != prev.advanced.api_base_url
                || next.advanced.request_timeout_secs != prev.advanced.request_timeout_secs
                || next.advanced.upload_timeout_secs != prev.advanced.upload_timeout_secs
            {
                match build_api(&next) {
                    Ok(api) => {
                        if let Ok(mut g) = self.inner.api.write() {
                            *g = api;
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "API の設定を反映できませんでした"),
                }
            }
            if next.advanced.history_limit != prev.advanced.history_limit {
                let mut h = lock(&self.inner.history);
                h.set_limit(next.advanced.history_limit);
                self.save_history(&h);
            }
            prev = next;
            self.publish();
        }
    }

    // ---- フォルダ監視 ----

    fn restart_watcher(&self) {
        let s = self.inner.settings.get();
        let started_at = {
            let mut g = lock(&self.inner.watch_started);
            match g.as_ref() {
                Some((folders, t)) if *folders == s.watch_folders => *t,
                _ => {
                    let now = SystemTime::now();
                    *g = Some((s.watch_folders.clone(), now));
                    now
                }
            }
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<ReadyFile>();
        let watcher = FolderWatcher::start(
            WatchOptions {
                folders: s.watch_folders.clone(),
                include_videos: s.watch_videos,
                stability: s.advanced.stability.clone(),
                started_at,
            },
            tx,
        );
        *lock(&self.inner.folders) = watcher.status().to_vec();
        *lock(&self.inner.watcher) = Some(watcher);
        let e = self.clone();
        tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                e.on_file_ready(&f.path, f.kind);
            }
        });
        self.publish();
    }

    /// 書き込みが終わった新しいファイル。
    fn on_file_ready(&self, path: &Path, kind: MediaKind) {
        tracing::debug!(file = %crate::logging::file_name_of(path), "書き込みが終わったファイルを受け取りました");
        let settings = self.inner.settings.get();
        let name = crate::logging::file_name_of(path);
        let Ok(fp) = Fingerprint::of(path) else {
            return;
        };
        {
            let mut p = lock(&self.inner.processed);
            if p.contains_path(&fp.path) {
                tracing::debug!(file = %name, "処理済みのファイル（内容の更新）のため無視します");
                return;
            }
            if !p.insert(fp) {
                tracing::debug!(file = %name, "送信済みのため無視します");
                return;
            }
            if let Err(e) = p.save() {
                tracing::warn!(error = %e, "処理済みの記録を保存できませんでした");
            }
        }
        if !settings.auto_upload_active() {
            tracing::info!(file = %name, "自動アップロードが無効のため送りません");
            return;
        }
        if self.is_blocked() {
            tracing::info!(file = %name, "利用が停止されているため送りません");
            return;
        }
        let now = Self::now();
        if lock(&self.inner.state).paused(now) {
            // 停止中に撮った写真は送らない（キューにも積まない）。
            tracing::info!(file = %name, "1日の上限に達しているため送りません");
            return;
        }
        let mut entry =
            HistoryEntry::new(path, kind, settings.default_quality, EntrySource::Auto, now);
        let id = entry.id.clone();
        if matches!(self.token(), Err(EngineError::NeedsRelogin)) {
            entry.status = EntryStatus::Failed;
            entry.failure = Some(Failure {
                code: "UNAUTHORIZED".into(),
                message: "連携が切れています。再ログインしてください".into(),
                retryable: true,
            });
            self.add_entry(entry);
            return;
        }
        self.add_entry(entry);
        self.enqueue(id, false);
    }

    fn add_entry(&self, entry: HistoryEntry) {
        {
            let mut h = lock(&self.inner.history);
            h.add(entry);
            self.save_history(&h);
        }
        self.publish();
    }

    // ---- 送信 ----

    async fn worker(self, mut rx: mpsc::UnboundedReceiver<Job>) {
        loop {
            let job = tokio::select! {
                () = self.inner.shutdown.cancelled() => return,
                j = rx.recv() => match j { Some(j) => j, None => return },
            };
            self.process(job).await;
        }
    }

    fn fail(&self, id: &str, code: &str, message: &str, retryable: bool) {
        let mut name = String::new();
        {
            let mut h = lock(&self.inner.history);
            h.update(id, |e| {
                e.status = EntryStatus::Failed;
                e.failure = Some(Failure {
                    code: code.to_owned(),
                    message: message.to_owned(),
                    retryable,
                });
                name = e.file_name.clone();
            });
            self.save_history(&h);
        }
        // 1日上限とキャンセルは想定内の停止なので、トレイの「エラーあり」にはしない。
        if code != "DAILY_LIMIT_EXCEEDED" && code != "CANCELLED" && code != "AUTO_UPLOAD_OFF" {
            self.inner.unseen_error.store(true, Ordering::SeqCst);
        }
        // イベントを受けた側が最新の状態を読めるよう、先にスナップショットを更新する。
        self.publish();
        self.emit(EngineEvent::Failed {
            entry_id: id.to_owned(),
            file_name: name,
            code: code.to_owned(),
            message: message.to_owned(),
        });
    }

    async fn process(&self, job: Job) {
        let id = job.entry_id;
        let Some(entry) = lock(&self.inner.history).get(&id).cloned() else {
            return;
        };
        if !matches!(entry.status, EntryStatus::Queued | EntryStatus::Waiting) {
            return;
        }
        let batch_cancel = entry
            .batch_id
            .as_ref()
            .and_then(|b| lock(&self.inner.batches).get(b).map(|b| b.cancel.clone()));
        if batch_cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            self.fail(&id, "CANCELLED", "キャンセルしました", true);
            self.after_entry(&entry);
            return;
        }
        let now = Self::now();
        if !job.manual
            && entry.source == EntrySource::Auto
            && !self.inner.settings.get().auto_upload_active()
        {
            self.fail(
                &id,
                "AUTO_UPLOAD_OFF",
                "自動アップロードが無効のため送りませんでした",
                true,
            );
            self.after_entry(&entry);
            return;
        }
        if !job.manual && entry.source == EntrySource::Auto && lock(&self.inner.state).paused(now) {
            self.fail(&id, "DAILY_LIMIT_EXCEEDED", "今日の上限に達しました", true);
            self.after_entry(&entry);
            return;
        }
        if !job.manual && entry.source == EntrySource::Auto && self.is_blocked() {
            self.fail(
                &id,
                "FORBIDDEN",
                "このアカウントではアップロードできません",
                false,
            );
            self.after_entry(&entry);
            return;
        }
        let token = match self.token() {
            Ok(t) => t,
            Err(_) => {
                self.fail(
                    &id,
                    "UNAUTHORIZED",
                    "連携が切れています。再ログインしてください",
                    true,
                );
                self.after_entry(&entry);
                return;
            }
        };
        if !entry.path.is_file() {
            self.fail(&id, "FILE_NOT_FOUND", "ファイルが見つかりません", false);
            self.after_entry(&entry);
            return;
        }
        {
            let mut h = lock(&self.inner.history);
            h.update(&id, |e| {
                e.status = EntryStatus::Uploading;
                e.failure = None;
            });
        }
        self.publish();
        self.emit(EngineEvent::UploadStarted {
            entry_id: id.clone(),
            file_name: entry.file_name.clone(),
            source: entry.source,
        });

        let cancel = batch_cancel
            .map(|c| c.child_token())
            .unwrap_or_else(|| self.inner.shutdown.child_token());
        *lock(&self.inner.current) = Some((id.clone(), cancel.clone()));
        let progress_engine = self.clone();
        let progress_id = id.clone();
        let progress: crate::api::ProgressFn = Arc::new(move |done, total| {
            lock(&progress_engine.inner.progress).insert(progress_id.clone(), (done, total));
        });
        let settings = self.inner.settings.get();
        let limits = lock(&self.inner.limits).clone();
        let api = self.api();
        let result = upload_file(
            &api,
            UploadJob {
                path: &entry.path,
                kind: entry.kind,
                quality: entry.quality,
                token: token.as_ref(),
                limits: limits.as_ref(),
            },
            &settings.advanced.retry,
            Some(progress),
            &cancel,
        )
        .await;
        *lock(&self.inner.current) = None;
        lock(&self.inner.progress).remove(&id);

        match result {
            Ok(up) => self.on_uploaded(&entry, up, token.is_none(), &settings),
            Err(e) => self.on_upload_error(&entry, e),
        }
        self.after_entry(&entry);
    }

    fn on_uploaded(
        &self,
        entry: &HistoryEntry,
        up: crate::upload::Uploaded,
        anonymous: bool,
        settings: &Settings,
    ) {
        let r = up.response;
        let processing = r.status == crate::api::RemoteStatus::Processing;
        {
            let mut h = lock(&self.inner.history);
            h.update(&entry.id, |e| {
                e.status = if processing {
                    EntryStatus::Processing
                } else {
                    EntryStatus::Done
                };
                e.remote_id = Some(r.id.clone());
                e.url = Some(r.url.clone());
                e.expires_at = Some(r.expires_at);
                // deleteToken は匿名のときだけ返る（契約 v1.1）。トークンありで返っても保持しない。
                e.delete_token = if anonymous {
                    r.delete_token.clone()
                } else {
                    None
                };
                e.anonymous = anonymous;
                e.shrunk = up.shrunk;
                e.failure = None;
            });
            self.save_history(&h);
        }
        self.set_online(true);
        if up.shrunk {
            self.emit(EngineEvent::Shrunk {
                file_name: entry.file_name.clone(),
                original_bytes: entry.size_bytes,
            });
        }
        let in_open_batch = entry.batch_id.as_ref().is_some_and(|b| {
            lock(&self.inner.batches)
                .get(b)
                .is_some_and(|b| !b.finished)
        });
        let (copied, copied_count) = if entry.source == EntrySource::Auto {
            if settings.copy_url_after_upload {
                self.copy_auto(&r.url, settings.advanced.toast_aggregate_ms)
            } else {
                (false, 0)
            }
        } else if in_open_batch {
            // まとめてコピーするのは右クリック共有の完了時。
            (false, 0)
        } else {
            // 履歴からの再試行など、まとまりの外で終わった手動の共有はその URL をコピーする。
            match self.inner.clipboard.set_text(&r.url) {
                Ok(()) => (true, 1),
                Err(e) => {
                    tracing::warn!(error = %e, "クリップボードにコピーできませんでした");
                    (false, 0)
                }
            }
        };
        self.publish();
        self.emit(EngineEvent::Uploaded {
            entry_id: entry.id.clone(),
            file_name: entry.file_name.clone(),
            url: r.url,
            source: entry.source,
            processing,
            copied,
            copied_count,
            kind: entry.kind,
        });
        let e = self.clone();
        tokio::spawn(async move { e.refresh_limits().await });
    }

    /// 自動アップロードの URL をコピーする。連続撮影はまとめて改行区切りにする。
    fn copy_auto(&self, url: &str, window_ms: u64) -> (bool, usize) {
        let joined = {
            let mut b = lock(&self.inner.clip_batch);
            let within = b
                .last
                .is_some_and(|t| t.elapsed() <= Duration::from_millis(window_ms));
            if !within {
                b.urls.clear();
            }
            b.urls.push(url.to_owned());
            b.last = Some(Instant::now());
            (join_urls(&b.urls), b.urls.len())
        };
        match self.inner.clipboard.set_text(&joined.0) {
            Ok(()) => (true, joined.1),
            Err(e) => {
                tracing::warn!(error = %e, "クリップボードにコピーできませんでした");
                (false, 0)
            }
        }
    }

    fn on_upload_error(&self, entry: &HistoryEntry, err: UploadError) {
        let id = &entry.id;
        match &err {
            UploadError::Api(ApiError::Unauthorized) => {
                self.fail(id, err.code(), &err.user_message(), true);
                self.mark_needs_relogin();
            }
            UploadError::Api(ApiError::Forbidden) => {
                self.fail(id, err.code(), &err.user_message(), false);
                self.set_blocked(true);
            }
            UploadError::Api(ApiError::DailyLimitExceeded {
                resets_at,
                daily_limit,
            }) => {
                let anonymous = lock(&self.inner.state).state.account.is_none();
                let newly_paused = {
                    let mut st = lock(&self.inner.state);
                    let was = st.paused(Self::now());
                    st.state.daily_pause = Some(DailyPause::start(*resets_at, Self::now()));
                    self.save_state(&st);
                    !was
                };
                self.fail(id, err.code(), "今日の上限に達しました", true);
                tracing::warn!("1日の上限に達したため自動アップロードを停止します");
                if newly_paused {
                    let limit = daily_limit
                        .filter(|&d| d != 0)
                        .or_else(|| lock(&self.inner.limits).as_ref().and_then(Limits::daily));
                    self.emit(EngineEvent::DailyLimitReached { anonymous, limit });
                }
            }
            e if e.is_offline() => {
                {
                    let mut h = lock(&self.inner.history);
                    h.update(id, |e| e.status = EntryStatus::Waiting);
                    self.save_history(&h);
                }
                self.set_offline();
            }
            UploadError::Api(ApiError::Cancelled) => {
                self.fail(id, "CANCELLED", "キャンセルしました", true);
            }
            UploadError::VideoTooLarge { .. } => {
                self.fail(id, err.code(), &err.user_message(), false);
                self.emit(EngineEvent::VideoTooLarge {
                    file_name: entry.file_name.clone(),
                    size_bytes: entry.size_bytes,
                });
            }
            _ => {
                tracing::warn!(code = err.code(), "アップロードに失敗しました");
                self.fail(id, err.code(), &err.user_message(), err.retryable());
            }
        }
    }

    fn after_entry(&self, entry: &HistoryEntry) {
        self.publish();
        if let Some(b) = &entry.batch_id {
            self.check_batch(b);
        }
    }

    // ---- オフライン ----

    fn set_online(&self, online: bool) {
        let was = self.inner.online.swap(online, Ordering::SeqCst);
        if online && !was {
            tracing::info!("接続が戻りました");
            self.emit(EngineEvent::BackOnline);
            let waiting: Vec<String> = lock(&self.inner.history)
                .entries()
                .iter()
                .rev()
                .filter(|e| e.status == EntryStatus::Waiting)
                .map(|e| e.id.clone())
                .collect();
            for id in waiting {
                self.enqueue(id, false);
            }
            self.publish();
        }
    }

    fn set_offline(&self) {
        let was_online = self.inner.online.swap(false, Ordering::SeqCst);
        if was_online {
            let waiting = lock(&self.inner.history)
                .entries()
                .iter()
                .filter(|e| e.status == EntryStatus::Waiting)
                .count();
            tracing::warn!(waiting, "オフラインになりました");
            self.emit(EngineEvent::Offline { waiting });
        }
        self.publish();
        if !self.inner.probing.swap(true, Ordering::SeqCst) {
            tokio::spawn(self.clone().probe_until_online());
        }
    }

    async fn probe_until_online(self) {
        loop {
            let interval = Duration::from_secs(
                self.inner
                    .settings
                    .get()
                    .advanced
                    .offline_probe_interval_secs,
            );
            tokio::select! {
                () = self.inner.shutdown.cancelled() => break,
                () = tokio::time::sleep(interval) => {}
            }
            let token = self.token().ok().flatten();
            match self.api().me(token.as_ref()).await {
                Err(e) if e.retry_class() == crate::api::RetryClass::WaitForConnection => continue,
                Err(ApiError::Timeout) => continue,
                _ => {
                    self.set_online(true);
                    break;
                }
            }
        }
        self.inner.probing.store(false, Ordering::SeqCst);
    }

    // ---- 1日上限 ----

    fn expire_pause(&self) -> bool {
        let mut st = lock(&self.inner.state);
        let expired = st.expire_pause(Self::now());
        if expired {
            self.save_state(&st);
            tracing::info!("1日の上限がリセットされたため自動アップロードを再開します");
        }
        expired
    }

    async fn pause_timer(self) {
        loop {
            tokio::select! {
                () = self.inner.shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
            if self.expire_pause() {
                self.publish();
            }
            // 期限切れになった匿名の削除用トークンを履歴ファイルから消す。
            let purged = {
                let mut h = lock(&self.inner.history);
                let changed = h.purge_expired_secrets(Self::now());
                if changed {
                    self.save_history(&h);
                }
                changed
            };
            if purged {
                self.publish();
            }
        }
    }

    // ---- 履歴の操作 ----

    /// URL をもう一度コピーする。
    pub fn copy_url(&self, entry_id: &str) -> Result<(), EngineError> {
        let url = {
            let h = lock(&self.inner.history);
            let e = h.get(entry_id).ok_or(EngineError::EntryNotFound)?;
            if !e.url_usable(Self::now()) {
                return Err(EngineError::InvalidState);
            }
            e.url.clone().ok_or(EngineError::InvalidState)?
        };
        self.inner
            .clipboard
            .set_text(&url)
            .map_err(|_| EngineError::Clipboard)
    }

    pub fn entry(&self, entry_id: &str) -> Option<HistoryEntry> {
        lock(&self.inner.history).get(entry_id).cloned()
    }

    /// 失敗した項目を送り直す（手動の操作として扱う）。
    pub fn retry(&self, entry_id: &str) -> Result<(), EngineError> {
        {
            let mut h = lock(&self.inner.history);
            let e = h.get(entry_id).ok_or(EngineError::EntryNotFound)?;
            let ok =
                e.status == EntryStatus::Failed && e.failure.as_ref().is_none_or(|f| f.retryable);
            if !ok {
                return Err(EngineError::InvalidState);
            }
            h.update(entry_id, |e| {
                e.status = EntryStatus::Queued;
                e.failure = None;
            });
            self.save_history(&h);
        }
        self.enqueue(entry_id.to_owned(), true);
        self.publish();
        Ok(())
    }

    /// サーバーから削除する。
    pub async fn delete_remote(&self, entry_id: &str) -> Result<(), EngineError> {
        let entry = self.entry(entry_id).ok_or(EngineError::EntryNotFound)?;
        let remote_id = entry.remote_id.clone().ok_or(EngineError::InvalidState)?;
        let result = if entry.anonymous {
            let dt = entry
                .delete_token
                .clone()
                .ok_or(EngineError::InvalidState)?;
            self.api()
                .delete_quick(&remote_id, DeleteAuth::DeleteToken(&dt))
                .await
        } else {
            let token = self.token()?.ok_or(EngineError::NeedsRelogin)?;
            self.api()
                .delete_quick(&remote_id, DeleteAuth::Bearer(&token))
                .await
        };
        match result {
            Ok(()) | Err(ApiError::NotFound) => {}
            Err(ApiError::Unauthorized) => {
                if !entry.anonymous {
                    self.mark_needs_relogin();
                }
                return Err(EngineError::Api(ApiError::Unauthorized));
            }
            Err(e) => return Err(e.into()),
        }
        {
            let mut h = lock(&self.inner.history);
            h.update(entry_id, |e| {
                e.status = EntryStatus::Deleted;
                e.delete_token = None;
            });
            self.save_history(&h);
        }
        tracing::info!("共有をサーバーから削除しました");
        self.publish();
        Ok(())
    }

    /// 履歴から消す（サーバーの削除はしない）。
    pub fn remove_entry(&self, entry_id: &str) -> Result<(), EngineError> {
        {
            let mut h = lock(&self.inner.history);
            let e = h.get(entry_id).ok_or(EngineError::EntryNotFound)?;
            if matches!(e.status, EntryStatus::Uploading) {
                return Err(EngineError::InvalidState);
            }
            h.remove(entry_id);
            self.save_history(&h);
        }
        self.publish();
        Ok(())
    }

    // ---- 右クリック共有 ----

    /// ファイルをまとめて共有する。進捗は [`Engine::batch`] で見る。
    pub fn share_files(&self, paths: &[PathBuf], quality: Option<Quality>) -> String {
        let batch_id = new_id();
        let settings = self.inner.settings.get();
        let quality = quality.unwrap_or(settings.default_quality);
        let now = Self::now();
        let mut items = Vec::new();
        let mut ids = Vec::new();
        for p in paths {
            let shown = crate::logging::file_name_of(p);
            match validate_share_path(p) {
                Ok((path, kind)) => {
                    let mut e = HistoryEntry::new(&path, kind, quality, EntrySource::Manual, now);
                    e.batch_id = Some(batch_id.clone());
                    if let Ok(fp) = Fingerprint::of(&path) {
                        let mut set = lock(&self.inner.processed);
                        set.insert(fp);
                        let _ = set.save();
                    }
                    items.push(BatchItem {
                        file_name: e.file_name.clone(),
                        entry_id: Some(e.id.clone()),
                        rejected: None,
                    });
                    ids.push(e.id.clone());
                    lock(&self.inner.history).add(e);
                }
                Err(reason) => {
                    tracing::warn!(file = %shown, reason = %reason, "共有できないファイルを受け取りました");
                    items.push(BatchItem {
                        file_name: shown,
                        entry_id: None,
                        rejected: Some(reason.to_string()),
                    });
                }
            }
        }
        {
            let h = lock(&self.inner.history);
            self.save_history(&h);
        }
        lock(&self.inner.batches).insert(
            batch_id.clone(),
            Batch {
                items,
                cancel: CancellationToken::new(),
                finished: false,
            },
        );
        tracing::info!(
            files = paths.len(),
            accepted = ids.len(),
            "右クリック共有を受け付けました"
        );
        self.emit(EngineEvent::BatchStarted {
            batch_id: batch_id.clone(),
        });
        for id in ids {
            self.enqueue(id, true);
        }
        self.publish();
        self.check_batch(&batch_id);
        batch_id
    }

    /// 進捗。
    pub fn batch(&self, batch_id: &str) -> Option<BatchView> {
        let items = {
            let b = lock(&self.inner.batches);
            let batch = b.get(batch_id)?;
            (batch.items.clone(), batch.finished)
        };
        let h = lock(&self.inner.history);
        let progress = lock(&self.inner.progress);
        let mut urls = Vec::new();
        let views = items
            .0
            .iter()
            .map(|it| {
                if let Some(r) = &it.rejected {
                    return BatchItemView {
                        file_name: it.file_name.clone(),
                        status: BatchItemStatus::Failed(r.clone()),
                        progress: 0.0,
                    };
                }
                let e = it.entry_id.as_ref().and_then(|id| h.get(id));
                let (status, p) = match e {
                    None => (BatchItemStatus::Failed("履歴から消されました".into()), 0.0),
                    Some(e) => match e.status {
                        EntryStatus::Queued | EntryStatus::Waiting => {
                            (BatchItemStatus::Waiting, 0.0)
                        }
                        EntryStatus::Uploading => {
                            let p = progress
                                .get(&e.id)
                                .map(|&(d, t)| if t == 0 { 0.0 } else { d as f32 / t as f32 })
                                .unwrap_or(0.0);
                            (BatchItemStatus::Uploading, p)
                        }
                        EntryStatus::Done | EntryStatus::Processing => {
                            if let Some(u) = &e.url {
                                urls.push(u.clone());
                            }
                            (BatchItemStatus::Done, 1.0)
                        }
                        EntryStatus::Failed | EntryStatus::Deleted => (
                            BatchItemStatus::Failed(
                                e.failure
                                    .as_ref()
                                    .map(|f| f.message.clone())
                                    .unwrap_or_else(|| "失敗".into()),
                            ),
                            0.0,
                        ),
                    },
                };
                BatchItemView {
                    file_name: it.file_name.clone(),
                    status,
                    progress: p,
                }
            })
            .collect();
        Some(BatchView {
            items: views,
            finished: items.1,
            urls,
        })
    }

    /// 共有を中止する（送信中のものも止める）。
    pub fn cancel_batch(&self, batch_id: &str) {
        let ids: Vec<String> = {
            let b = lock(&self.inner.batches);
            let Some(batch) = b.get(batch_id) else { return };
            batch.cancel.cancel();
            batch
                .items
                .iter()
                .filter_map(|i| i.entry_id.clone())
                .collect()
        };
        {
            let mut h = lock(&self.inner.history);
            for id in &ids {
                h.update(id, |e| {
                    if matches!(e.status, EntryStatus::Queued | EntryStatus::Waiting) {
                        e.status = EntryStatus::Failed;
                        e.failure = Some(Failure {
                            code: "CANCELLED".into(),
                            message: "キャンセルしました".into(),
                            retryable: true,
                        });
                    }
                });
            }
            self.save_history(&h);
        }
        tracing::info!("右クリック共有をキャンセルしました");
        self.publish();
        self.check_batch(batch_id);
    }

    /// すべて終わっていれば URL を改行区切りでコピーして完了を通知する。
    fn check_batch(&self, batch_id: &str) {
        let Some(view) = self.batch(batch_id) else {
            return;
        };
        if view.finished {
            return;
        }
        let done = view
            .items
            .iter()
            .all(|i| matches!(i.status, BatchItemStatus::Done | BatchItemStatus::Failed(_)));
        if !done {
            return;
        }
        {
            let mut batches = lock(&self.inner.batches);
            match batches.get_mut(batch_id) {
                Some(b) if !b.finished => b.finished = true,
                _ => return,
            }
        }
        let succeeded = view.urls.len();
        let failed = view.items.len() - succeeded;
        let copied = if succeeded > 0 {
            match self.inner.clipboard.set_text(&join_urls(&view.urls)) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "クリップボードにコピーできませんでした");
                    false
                }
            }
        } else {
            false
        };
        tracing::info!(succeeded, failed, "右クリック共有が終わりました");
        self.emit(EngineEvent::BatchFinished {
            batch_id: batch_id.to_owned(),
            succeeded,
            failed,
            copied,
        });
    }

    // ---- アンインストール ----

    /// アンインストール前の後片付け。
    pub async fn cleanup_for_uninstall(
        &self,
        opts: UninstallOptions,
        on_step: impl Fn(UninstallStep),
    ) -> UninstallReport {
        self.shutdown();
        let mut report = UninstallReport::default();
        if opts.delete_uploads {
            on_step(UninstallStep::DeletingUploads);
            let ids: Vec<String> = lock(&self.inner.history)
                .entries()
                .iter()
                .filter(|e| e.remote_id.is_some() && e.url_usable(Self::now()))
                .map(|e| e.id.clone())
                .collect();
            for id in ids {
                if let Err(e) = self.delete_remote(&id).await {
                    tracing::warn!(error = %e, "アップロード済みファイルを削除できませんでした");
                    report.uploads_not_deleted += 1;
                }
            }
        }
        if opts.unlink {
            on_step(UninstallStep::RevokingToken);
            report.server_revoked = match self.logout().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "連携の解除に失敗しました");
                    false
                }
            };
        } else {
            report.server_revoked = true;
        }
        // 資格情報マネージャーのトークンは、サーバー側の解除を選ばなくても必ず削除する（INSTRUCTIONS §5）。
        if let Err(e) = self.inner.token_store.delete() {
            tracing::warn!(error = %e, "資格情報マネージャーのトークンを削除できませんでした");
        }
        // SteamVR の登録ファイル（.vrmanifest）は残さない。
        let _ = std::fs::remove_dir_all(self.inner.paths.root().join("steamvr"));
        if opts.delete_local_data {
            on_step(UninstallStep::DeletingFiles);
            let root = self.inner.paths.root().to_path_buf();
            for f in [
                self.inner.paths.settings_file(),
                self.inner.paths.history_file(),
                self.inner.paths.processed_file(),
                self.inner.paths.state_file(),
                self.inner.paths.update_cache_file(),
                self.inner.paths.shell_flag_file(),
            ] {
                let _ = std::fs::remove_file(f);
            }
            let _ = std::fs::remove_dir_all(self.inner.paths.download_dir());
            // 退避した壊れたファイル・書き込み途中の一時ファイルも残さない。
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.ends_with(".broken.json") || (n.starts_with('.') && n.ends_with(".tmp")) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            report.local_data_deleted = true;
            tracing::info!(root = %crate::logging::mask_path(&root), "ローカルの設定と履歴を削除しました");
        }
        report
    }
}

/// アンインストール時に一緒に行うこと（画面 22）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UninstallOptions {
    pub delete_local_data: bool,
    pub unlink: bool,
    pub delete_uploads: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallStep {
    DeletingUploads,
    RevokingToken,
    DeletingFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UninstallReport {
    pub server_revoked: bool,
    pub uploads_not_deleted: usize,
    pub local_data_deleted: bool,
}

/// 進行中のブラウザ認可。
#[derive(Debug)]
pub struct LoginSession {
    engine: Engine,
    pending: PendingAuth,
    cancel: CancellationToken,
}

impl LoginSession {
    pub fn connection_code(&self) -> &str {
        self.pending.connection_code()
    }
    pub fn browser_url(&self) -> &url::Url {
        self.pending.browser_url()
    }
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// ブラウザでの許可を待ってトークンを保存する。
    pub async fn finish(self) -> Result<User, AuthError> {
        let timeout = Duration::from_secs(self.engine.settings().get().advanced.auth_timeout_secs);
        let code = self.pending.wait(timeout, &self.cancel).await?;
        let api = self.engine.api();
        let resp = self.pending.exchange(&api, code).await?;
        self.engine.complete_login(resp).await
    }
}
