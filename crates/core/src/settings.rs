//! 設定（永続化・検証・即時反映）。
//!
//! 設定は `settings.json` に保存する。値を変えるときは必ず [`SettingsStore::update`] を通し、
//! 検証 → 保存 → 購読者への通知 の順で反映する（再起動不要）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::error::StorageError;
use crate::fsutil;

/// 画質。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// 原寸（高画質WebPで再エンコード、埋め込み情報は除去）。
    Original,
    /// 縮小（長辺2000px WebP）。既定（Q-011 確定: 2026-11-21）。
    #[default]
    Shrink,
}

impl Quality {
    /// API の `quality` パラメーター値。
    pub fn as_api(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Shrink => "shrink",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "原寸",
            Self::Shrink => "縮小",
        }
    }
}

/// ログレベル。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    pub fn as_filter(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

/// 再試行の設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RetrySettings {
    /// 1件あたりの最大試行回数（初回を含む）。
    pub max_attempts: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff_ms: 2_000,
            max_backoff_ms: 60_000,
        }
    }
}

/// 書き込み完了の判定（安定待ち）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StabilitySettings {
    pub poll_interval_ms: u64,
    /// サイズと更新時刻がこの時間変わらなければ書き込み完了とみなす。
    pub stable_for_ms: u64,
    /// これを超えても安定しなければ諦める。
    pub max_wait_ms: u64,
    /// 通知の取りこぼしに備えてフォルダを走査し直す間隔（0 で無効）。
    pub rescan_interval_ms: u64,
}

impl Default for StabilitySettings {
    fn default() -> Self {
        Self {
            poll_interval_ms: 500,
            stable_for_ms: 1_500,
            max_wait_ms: 120_000,
            rescan_interval_ms: 15_000,
        }
    }
}

/// 通知の設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NotificationSettings {
    pub windows_toast: bool,
    pub vr_hud: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            windows_toast: true,
            vr_hud: true,
        }
    }
}

/// 内部用の設定（ハードコードせず設定ファイルで変更できる値）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AdvancedSettings {
    /// API のベースURL。
    pub api_base_url: String,
    pub log_level: LogLevel,
    pub history_limit: usize,
    pub retry: RetrySettings,
    pub stability: StabilitySettings,
    pub update_check_interval_hours: u64,
    pub request_timeout_secs: u64,
    pub upload_timeout_secs: u64,
    /// 連続撮影時に通知をまとめる時間。
    pub toast_aggregate_ms: u64,
    pub offline_probe_interval_secs: u64,
    /// ブラウザ認可の待ち時間。
    pub auth_timeout_secs: u64,
    /// Web の「連携アプリ」管理ページのパス（API と同じホスト）。
    pub manage_apps_path: String,
    /// ダウンロードページ（更新履歴）のパス。
    pub download_page_path: String,
    /// 403（BAN など）のときに案内するお問い合わせページのパス（API と同じホスト）。
    pub contact_path: String,
}

pub const DEFAULT_API_BASE_URL: &str = "https://clipshare.link";

/// 403 の案内先（お問い合わせページ）の既定パス。既定の API ベース URL と合わせて
/// `https://clipshare.link/contact`（QUESTIONS Q-027、オーナー確定 2026-11-21）。
/// 設定の `advanced.contactPath` でも変更できる（同じホストのパスのみ）。
pub const DEFAULT_CONTACT_PATH: &str = "/contact";

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            api_base_url: DEFAULT_API_BASE_URL.to_owned(),
            log_level: LogLevel::Info,
            history_limit: 50,
            retry: RetrySettings::default(),
            stability: StabilitySettings::default(),
            update_check_interval_hours: 24,
            request_timeout_secs: 30,
            upload_timeout_secs: 600,
            toast_aggregate_ms: 4_000,
            offline_probe_interval_secs: 30,
            auth_timeout_secs: 300,
            manage_apps_path: "/settings/apps".to_owned(),
            download_page_path: "/qick/download".to_owned(),
            contact_path: DEFAULT_CONTACT_PATH.to_owned(),
        }
    }
}

/// アプリの設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub schema_version: u32,
    /// 初回セットアップを終えたか。
    pub onboarding_completed: bool,
    /// 自動アップロード（撮影した写真を送ること）に同意したか。未同意なら自動では何も送らない。
    pub auto_upload_consented: bool,
    pub auto_upload: bool,
    pub copy_url_after_upload: bool,
    pub default_quality: Quality,
    pub watch_folders: Vec<PathBuf>,
    /// 動画も自動アップロードの対象にするか。
    pub watch_videos: bool,
    pub launch_at_startup: bool,
    pub auto_update_check: bool,
    pub context_menu_enabled: bool,
    pub steamvr_enabled: bool,
    pub notifications: NotificationSettings,
    pub advanced: AdvancedSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            onboarding_completed: false,
            auto_upload_consented: false,
            auto_upload: true,
            copy_url_after_upload: true,
            // 既定は「縮小（長辺2000px）」（Q-011）。保存済みの設定には必ずこの項目が書かれるため、
            // 既存の利用者の選択は変わらない（マイグレーション不要）。
            default_quality: Quality::Shrink,
            watch_folders: Vec::new(),
            watch_videos: false,
            launch_at_startup: true,
            auto_update_check: true,
            context_menu_enabled: true,
            steamvr_enabled: true,
            notifications: NotificationSettings::default(),
            advanced: AdvancedSettings::default(),
        }
    }
}

/// 設定値の検証エラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("APIのURLが不正です（https のみ使えます）")]
    InvalidApiUrl,
    #[error("写真フォルダはローカルの絶対パスで指定してください")]
    InvalidFolder(PathBuf),
    #[error("同じフォルダが重複しています")]
    DuplicateFolder(PathBuf),
    #[error("{0} の値が範囲外です")]
    OutOfRange(&'static str),
}

impl Settings {
    /// 自動アップロードが実際に動く条件（同意済みかつON）。
    pub fn auto_upload_active(&self) -> bool {
        self.auto_upload_consented && self.auto_upload
    }

    /// 値を検証する。保存前に必ず呼ばれる。
    pub fn validate(&self) -> Result<(), SettingsError> {
        validate_api_url(&self.advanced.api_base_url)?;
        let mut seen: Vec<String> = Vec::new();
        for f in &self.watch_folders {
            if !crate::pathcheck::is_local_absolute(f) {
                return Err(SettingsError::InvalidFolder(f.clone()));
            }
            let key = folder_key(f);
            if seen.contains(&key) {
                return Err(SettingsError::DuplicateFolder(f.clone()));
            }
            seen.push(key);
        }
        let a = &self.advanced;
        range("historyLimit", a.history_limit as u64, 1, 500)?;
        range("retry.maxAttempts", u64::from(a.retry.max_attempts), 1, 20)?;
        range(
            "retry.initialBackoffMs",
            a.retry.initial_backoff_ms,
            100,
            600_000,
        )?;
        range(
            "retry.maxBackoffMs",
            a.retry.max_backoff_ms,
            a.retry.initial_backoff_ms,
            3_600_000,
        )?;
        range(
            "stability.pollIntervalMs",
            a.stability.poll_interval_ms,
            10,
            10_000,
        )?;
        range(
            "stability.stableForMs",
            a.stability.stable_for_ms,
            10,
            60_000,
        )?;
        range(
            "stability.maxWaitMs",
            a.stability.max_wait_ms,
            a.stability.stable_for_ms,
            3_600_000,
        )?;
        range(
            "updateCheckIntervalHours",
            a.update_check_interval_hours,
            1,
            24 * 30,
        )?;
        range("requestTimeoutSecs", a.request_timeout_secs, 1, 600)?;
        range("uploadTimeoutSecs", a.upload_timeout_secs, 1, 7_200)?;
        range("toastAggregateMs", a.toast_aggregate_ms, 0, 60_000)?;
        range(
            "offlineProbeIntervalSecs",
            a.offline_probe_interval_secs,
            1,
            3_600,
        )?;
        range("authTimeoutSecs", a.auth_timeout_secs, 10, 1_800)?;
        for (name, path) in [
            ("manageAppsPath", &a.manage_apps_path),
            ("downloadPagePath", &a.download_page_path),
            ("contactPath", &a.contact_path),
        ] {
            let ok = path.starts_with('/')
                && !path.starts_with("//")
                && path.len() <= 200
                && path
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "/-_.".contains(c));
            if !ok {
                return Err(SettingsError::OutOfRange(name));
            }
        }
        Ok(())
    }
}

/// フォルダ比較用のキー（大文字小文字・末尾区切りを無視）。
pub fn folder_key(p: &Path) -> String {
    p.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

fn range(name: &'static str, v: u64, min: u64, max: u64) -> Result<(), SettingsError> {
    if v < min || v > max {
        Err(SettingsError::OutOfRange(name))
    } else {
        Ok(())
    }
}

/// API のURLを検証する。https のみ。開発用にループバック（127.0.0.1 / [::1] / localhost）の http は許可。
pub fn validate_api_url(s: &str) -> Result<url::Url, SettingsError> {
    let u = url::Url::parse(s).map_err(|_| SettingsError::InvalidApiUrl)?;
    if u.cannot_be_a_base() || !u.username().is_empty() || u.password().is_some() {
        return Err(SettingsError::InvalidApiUrl);
    }
    match u.scheme() {
        "https" => Ok(u),
        "http" if is_loopback_host(&u) => Ok(u),
        _ => Err(SettingsError::InvalidApiUrl),
    }
}

pub(crate) fn is_loopback_host(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// 設定の読み書きと変更通知。
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
    current: Arc<RwLock<Settings>>,
    tx: Arc<watch::Sender<Settings>>,
}

impl SettingsStore {
    /// 読み込む。無ければ既定値、壊れていれば退避して既定値で始める。
    pub fn load(path: &Path) -> Self {
        let settings = match fsutil::load_json::<Settings>(path) {
            Ok(Some(s)) => match s.validate() {
                Ok(()) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "設定値が不正なため既定値に戻します");
                    fsutil::quarantine(path);
                    Settings::default()
                }
            },
            Ok(None) => Settings::default(),
            Err(e) => {
                tracing::warn!(error = %e, "設定を読み込めないため既定値で起動します");
                fsutil::quarantine(path);
                Settings::default()
            }
        };
        Self::from_settings(path, settings)
    }

    /// 指定の値で作る（テスト用・初期化用）。ファイルには書かない。
    pub fn from_settings(path: &Path, settings: Settings) -> Self {
        let (tx, _rx) = watch::channel(settings.clone());
        Self {
            path: path.to_path_buf(),
            current: Arc::new(RwLock::new(settings)),
            tx: Arc::new(tx),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self) -> Settings {
        match self.current.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<Settings> {
        self.tx.subscribe()
    }

    /// 設定を変更する。検証に失敗したら何も変えない。
    pub fn update<F>(&self, f: F) -> Result<Settings, SettingsUpdateError>
    where
        F: FnOnce(&mut Settings),
    {
        let mut guard = match self.current.write() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut next = guard.clone();
        f(&mut next);
        next.validate()?;
        if next != *guard {
            fsutil::save_json(&self.path, &next)?;
            *guard = next.clone();
            self.tx.send_replace(next.clone());
            tracing::info!("設定を保存しました");
        }
        Ok(next)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsUpdateError {
    #[error(transparent)]
    Invalid(#[from] SettingsError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_auto_upload_needs_consent() {
        let s = Settings::default();
        s.validate().unwrap();
        assert!(s.auto_upload);
        assert!(
            !s.auto_upload_active(),
            "未同意なら自動アップロードは動かない"
        );
        assert_eq!(s.advanced.history_limit, 50);
    }

    #[test]
    fn api_url_validation() {
        assert!(validate_api_url("https://clipshare.link").is_ok());
        assert!(validate_api_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_api_url("http://[::1]:8080").is_ok());
        assert!(validate_api_url("http://localhost:1").is_ok());
        assert!(validate_api_url("http://clipshare.link").is_err());
        assert!(validate_api_url("ftp://x").is_err());
        assert!(validate_api_url("https://user:pw@clipshare.link").is_err());
        assert!(validate_api_url("not a url").is_err());
    }

    #[test]
    fn invalid_values_are_rejected_and_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let store = SettingsStore::load(&p);
        let err = store.update(|s| s.advanced.history_limit = 0).unwrap_err();
        assert!(matches!(
            err,
            SettingsUpdateError::Invalid(SettingsError::OutOfRange(_))
        ));
        assert!(!p.exists(), "不正値は保存しない");
        assert_eq!(store.get().advanced.history_limit, 50);

        let err = store
            .update(|s| s.watch_folders = vec![PathBuf::from(r"\\server\share")])
            .unwrap_err();
        assert!(matches!(
            err,
            SettingsUpdateError::Invalid(SettingsError::InvalidFolder(_))
        ));
        let err = store
            .update(|s| s.watch_folders = vec![PathBuf::from(r"C:\a"), PathBuf::from(r"c:\A\")])
            .unwrap_err();
        assert!(matches!(
            err,
            SettingsUpdateError::Invalid(SettingsError::DuplicateFolder(_))
        ));
    }

    #[test]
    fn update_persists_and_notifies() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let store = SettingsStore::load(&p);
        let mut rx = store.subscribe();
        store
            .update(|s| s.default_quality = Quality::Original)
            .unwrap();
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow_and_update().default_quality, Quality::Original);
        let reloaded = SettingsStore::load(&p);
        assert_eq!(reloaded.get().default_quality, Quality::Original);
    }

    #[test]
    fn broken_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, b"{\"advanced\":{\"historyLimit\":0}}").unwrap();
        let store = SettingsStore::load(&p);
        assert_eq!(store.get(), Settings::default());
        std::fs::write(&p, b"garbage").unwrap();
        let store = SettingsStore::load(&p);
        assert_eq!(store.get(), Settings::default());
    }

    #[test]
    fn web_paths_must_be_same_host_paths() {
        let mut s = Settings::default();
        s.advanced.manage_apps_path = "//evil.example/x".into();
        assert!(s.validate().is_err());
        s.advanced.manage_apps_path = "https://evil.example".into();
        assert!(s.validate().is_err());
        s.advanced.manage_apps_path = "/settings/apps".into();
        assert!(s.validate().is_ok());
    }

    #[test]
    fn default_quality_is_shrink_but_saved_choice_is_kept() {
        assert_eq!(Settings::default().default_quality, Quality::Shrink);
        // 既存の利用者が保存した「原寸」はそのまま
        let s: Settings = serde_json::from_str(r#"{"defaultQuality":"original"}"#).unwrap();
        assert_eq!(s.default_quality, Quality::Original);
        // 項目が無い（初回・手書き）場合は既定の縮小
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.default_quality, Quality::Shrink);
        // 保存すると必ず書き出される（次回読み込み時に既定値の変更の影響を受けない）
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains("\"defaultQuality\":\"shrink\""));
        // 破損からの復旧も既定の縮小
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, b"broken").unwrap();
        assert_eq!(
            SettingsStore::load(&p).get().default_quality,
            Quality::Shrink
        );
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let s: Settings = serde_json::from_str(r#"{"autoUpload":false,"futureField":1}"#).unwrap();
        assert!(!s.auto_upload);
        assert_eq!(s.advanced.retry, RetrySettings::default());
    }
}
