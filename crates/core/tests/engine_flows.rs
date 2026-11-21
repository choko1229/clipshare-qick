//! エンジンの結合テスト（契約どおりのモックサーバーを相手にする）。
//!
//! INSTRUCTIONS §9 の自動テスト項目: 監視（書き込み途中・重複・既存写真）、アップロード（成功・413・429・401・
//! 5xx/タイムアウト・失敗の永続化・オフライン復帰）、履歴（再コピー・削除・再試行）、複数選択の URL 連結、認可。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cq_core::clipboard::MemoryClipboard;
use cq_core::engine::{AccountView, BatchItemStatus, Engine, EngineConfig, EngineEvent, Snapshot};
use cq_core::history::{DisplayStatus, EntryStatus};
use cq_core::paths::AppPaths;
use cq_core::settings::{Quality, Settings, SettingsStore};
use cq_core::token_store::{MemoryTokenStore, TokenStore};
use cq_mock_server::{AuthorizeMode, Endpoint, MockConfig, MockServer, Scripted};
use serde_json::json;
use tokio::sync::broadcast;

/// 同時に動かすテストの数を絞る（多数のエンジン・画像処理・ウイルス対策ソフトのスキャンが重なると、
/// 環境によって数十秒単位で止まり、タイムアウトで不安定になるため）。
static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();

async fn slot() -> tokio::sync::OwnedSemaphorePermit {
    let sem = SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone();
    match sem.acquire_owned().await {
        Ok(p) => p,
        Err(e) => panic!("{e}"),
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    photos: PathBuf,
    engine: Engine,
    clipboard: Arc<MemoryClipboard>,
    tokens: Arc<MemoryTokenStore>,
    events: broadcast::Receiver<EngineEvent>,
}

fn fast_settings(api: &str, photos: &Path) -> Settings {
    let mut s = Settings::default();
    s.onboarding_completed = true;
    s.auto_upload_consented = true;
    s.watch_folders = vec![photos.to_path_buf()];
    s.advanced.api_base_url = api.to_owned();
    s.advanced.retry.max_attempts = 3;
    s.advanced.retry.initial_backoff_ms = 100;
    s.advanced.retry.max_backoff_ms = 300;
    s.advanced.stability.poll_interval_ms = 20;
    s.advanced.stability.stable_for_ms = 120;
    s.advanced.stability.max_wait_ms = 20_000;
    s.advanced.stability.rescan_interval_ms = 1_000;
    s.advanced.offline_probe_interval_secs = 1;
    s.advanced.request_timeout_secs = 5;
    s.advanced.toast_aggregate_ms = 30_000;
    s
}

impl Harness {
    async fn new(api: &str, tweak: impl FnOnce(&mut Settings)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let photos = dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        Self::with_dirs(
            dir,
            root,
            photos,
            Arc::new(MemoryTokenStore::default()),
            api,
            tweak,
        )
        .await
    }

    async fn with_dirs(
        dir: tempfile::TempDir,
        root: PathBuf,
        photos: PathBuf,
        tokens: Arc<MemoryTokenStore>,
        api: &str,
        tweak: impl FnOnce(&mut Settings),
    ) -> Self {
        if std::env::var_os("CQ_TEST_LOG").is_some() {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("cq_core=debug")
                .with_thread_names(true)
                .try_init();
        }
        let paths = AppPaths::new(&root);
        let mut s = fast_settings(api, &photos);
        tweak(&mut s);
        let store = SettingsStore::from_settings(&paths.settings_file(), s);
        let clipboard = Arc::new(MemoryClipboard::default());
        let engine = Engine::start(EngineConfig {
            paths,
            settings: store,
            token_store: tokens.clone(),
            clipboard: clipboard.clone(),
        })
        .unwrap();
        let events = engine.subscribe();
        // 監視の開始を待つ
        tokio::time::sleep(Duration::from_millis(150)).await;
        Self {
            _dir: dir,
            root,
            photos,
            engine,
            clipboard,
            tokens,
            events,
        }
    }

    /// エンジンを作り直す（再起動の再現）。
    async fn restart(self, api: &str) -> Self {
        self.engine.shutdown();
        let Harness {
            _dir,
            root,
            photos,
            tokens,
            ..
        } = self;
        Self::with_dirs(_dir, root, photos, tokens, api, |_| {}).await
    }

    fn snap(&self) -> Snapshot {
        self.engine.snapshot().borrow().clone()
    }

    async fn wait_for(&self, what: &str, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let s = self.snap();
            if f(&s) {
                return s;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "待ちきれませんでした: {what}"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    async fn next_event(&mut self, f: impl Fn(&EngineEvent) -> bool) -> EngineEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.events.recv()).await {
                Ok(Ok(ev)) if f(&ev) => return ev,
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(e)) => panic!("{e}"),
                Err(_) => panic!("イベントが来ませんでした"),
            }
        }
    }

    fn photo(&self, name: &str, w: u32, h: u32) -> PathBuf {
        let sub = self.photos.join("2026-11");
        std::fs::create_dir_all(&sub).unwrap();
        let p = sub.join(name);
        write_png(&p, w, h);
        p
    }
}

fn write_png(p: &Path, w: u32, h: u32) {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([
            (x * 7 % 256) as u8,
            (y * 13 % 256) as u8,
            ((x ^ y) % 256) as u8,
        ])
    });
    // 一時名で書いてから名前を変える（書き込み途中を拾わないことの確認は watcher の単体テストで行う）
    let tmp = p.with_extension("tmp");
    img.save_with_format(&tmp, image::ImageFormat::Png).unwrap();
    std::fs::rename(&tmp, p).unwrap();
}

fn write_fake_video(p: &Path, len: usize) {
    let mut v = b"\0\0\0\x20ftypisom".to_vec();
    v.resize(len, 0);
    std::fs::write(p, v).unwrap();
}

/// ブラウザの代わりに認可URLを開き、リダイレクトをループバックへ届ける。
async fn act_as_browser(url: &url::Url) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = client.get(url.as_str()).send().await.unwrap();
    if r.status().is_redirection() {
        let loc = r
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let _ = client.get(loc).send().await.unwrap();
    }
}

async fn login(h: &Harness) {
    let session = h.engine.start_login().await.unwrap();
    let url = session.browser_url().clone();
    let (res, ()) = tokio::join!(session.finish(), act_as_browser(&url));
    let user = res.unwrap();
    assert_eq!(user.name, "choko1229");
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_upload_copies_url_and_records_history() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    h.photo("VRChat_2026-11-19_21-04-12.png", 64, 48);
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    let EngineEvent::Uploaded {
        url,
        copied,
        copied_count,
        ..
    } = ev
    else {
        unreachable!()
    };
    assert!(copied);
    assert_eq!(copied_count, 1);
    assert!(url.starts_with(&mock.base_url()));
    assert_eq!(h.clipboard.get().as_deref(), Some(url.as_str()));
    let s = h
        .wait_for("完了", |s| {
            s.history
                .first()
                .is_some_and(|e| e.status == EntryStatus::Done)
        })
        .await;
    let e = &s.history[0];
    assert!(e.anonymous);
    assert!(e.delete_token.is_some());
    let reqs = mock.requests_to(Endpoint::Quick);
    assert_eq!(reqs.len(), 1);
    // 既定の画質は「縮小」（Q-011）
    assert_eq!(reqs[0].quality.as_deref(), Some("shrink"));
    assert!(!reqs[0].authorized);
    assert!(reqs[0].user_agent.starts_with("ClipshareQuick/"));
    assert!(reqs[0].user_agent.ends_with("(Windows)"));
    // 再コピー
    h.clipboard.set_text_for_test("");
    h.engine.copy_url(&e.id).unwrap();
    assert_eq!(h.clipboard.get().as_deref(), Some(url.as_str()));
}

trait SetForTest {
    fn set_text_for_test(&self, s: &str);
}
impl SetForTest for MemoryClipboard {
    fn set_text_for_test(&self, s: &str) {
        use cq_core::clipboard::Clipboard;
        self.set_text(s).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_sent_without_consent_or_when_auto_upload_is_off() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let h = Harness::new(&mock.base_url(), |s| s.auto_upload_consented = false).await;
    h.photo("a.png", 8, 8);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(mock.requests_to(Endpoint::Quick).is_empty());
    assert!(h.snap().history.is_empty());

    let h2 = Harness::new(&mock.base_url(), |s| s.auto_upload = false).await;
    h2.photo("b.png", 8, 8);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(mock.requests_to(Endpoint::Quick).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_photos_are_ignored_and_duplicates_sent_once() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    let old = photos.join("old.png");
    write_png(&old, 8, 8);
    // 作成時刻の比較に余裕を持たせる
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let mut h = Harness::with_dirs(
        dir,
        tempfile::tempdir().unwrap().keep().join("data"),
        photos,
        Arc::new(MemoryTokenStore::default()),
        &mock.base_url(),
        |_| {},
    )
    .await;
    // 既存写真の更新（書き換え）は送らない
    write_png(&old, 9, 9);
    let p = h.photo("new.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    // 同じファイルへの重複イベント（属性の更新）では送らない
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_modified(std::fs::metadata(&p).unwrap().modified().unwrap())
        .unwrap();
    drop(f);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let reqs = mock.requests_to(Endpoint::Quick);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert_eq!(reqs[0].file_name.as_deref(), Some("new.png"));
}

#[tokio::test(flavor = "multi_thread")]
async fn image_413_is_shrunk_and_resent_video_413_is_an_error() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |s| s.watch_videos = true).await;
    mock.push_error(
        Endpoint::Quick,
        413,
        "SIZE_LIMIT_EXCEEDED",
        json!({ "limitBytes": 10, "kind": "IMAGE" }),
    );
    h.photo("big.png", 2400, 1200);
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::Shrunk { .. }))
        .await;
    assert!(matches!(ev, EngineEvent::Shrunk { ref file_name, .. } if file_name == "big.png"));
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    let reqs = mock.requests_to(Endpoint::Quick);
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].status, 413);
    assert_eq!(reqs[1].status, 201);
    assert_eq!(reqs[1].quality.as_deref(), Some("shrink"));
    let s = h
        .wait_for("完了", |s| s.history[0].status == EntryStatus::Done)
        .await;
    assert!(s.history[0].shrunk);

    mock.push_error(
        Endpoint::Quick,
        413,
        "SIZE_LIMIT_EXCEEDED",
        json!({ "limitBytes": 10, "kind": "VIDEO" }),
    );
    let v = h.photos.join("clip_0411.mp4");
    write_fake_video(&v, 4096);
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::VideoTooLarge { .. }))
        .await;
    assert!(
        matches!(ev, EngineEvent::VideoTooLarge { ref file_name, .. } if file_name == "clip_0411.mp4")
    );
    let s = h
        .wait_for("動画の失敗", |s| {
            s.history[0].status == EntryStatus::Failed
        })
        .await;
    let f = s.history[0].failure.clone().unwrap();
    assert_eq!(f.message, "動画は縮小できないため送れませんでした");
    assert!(!f.retryable);
    assert_eq!(
        mock.requests_to(Endpoint::Quick).len(),
        3,
        "動画は送り直さない"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn known_limits_are_checked_before_sending() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig {
        anon_video_max: 1000,
        ..MockConfig::default()
    })
    .await
    .unwrap();
    let mut h = Harness::new(&mock.base_url(), |s| s.watch_videos = true).await;
    // 起動時の取得が環境の遅延で失敗しても良いよう、明示的に取り直す。
    for _ in 0..20 {
        if h.snap().limits.is_some() {
            break;
        }
        h.engine.refresh_limits().await;
    }
    h.wait_for("上限の取得", |s| s.limits.is_some()).await;
    write_fake_video(&h.photos.join("long.mp4"), 4096);
    h.next_event(|e| matches!(e, EngineEvent::VideoTooLarge { .. }))
        .await;
    assert!(
        mock.requests_to(Endpoint::Quick).is_empty(),
        "上限を超える動画は送らない"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn daily_limit_pauses_and_photos_taken_meanwhile_are_never_sent() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig {
        anon_daily_limit: 1,
        ..MockConfig::default()
    })
    .await
    .unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    h.photo("1.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    h.photo("2.png", 8, 8);
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::DailyLimitReached { .. }))
        .await;
    assert!(matches!(
        ev,
        EngineEvent::DailyLimitReached {
            anonymous: true,
            ..
        }
    ));
    let s = h.wait_for("停止", |s| s.daily_paused).await;
    assert_eq!(s.tray, cq_core::engine::TrayState::Paused);
    // 停止中に撮った写真は送らない（履歴にも入れない）
    h.photo("3.png", 8, 8);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let names: Vec<_> = h
        .snap()
        .history
        .iter()
        .map(|e| e.file_name.clone())
        .collect();
    assert!(!names.contains(&"3.png".to_owned()), "{names:?}");
    assert_eq!(mock.requests_to(Endpoint::Quick).len(), 2);

    // 翌日（リセット時刻を過ぎた）を再現: 停止の期限を過去にして再起動
    mock.reset_daily();
    let state_file = h.root.join("state.json");
    let mut st: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
    st["dailyPause"] = json!({ "until": "2000-01-01T00:00:00Z", "localDate": null });
    std::fs::write(&state_file, st.to_string()).unwrap();
    let mut h = h.restart(&mock.base_url()).await;
    assert!(!h.snap().daily_paused);
    h.photo("4.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    let sent: Vec<_> = mock
        .requests_to(Endpoint::Quick)
        .into_iter()
        .filter_map(|r| r.file_name)
        .collect();
    assert!(
        !sent.contains(&"3.png".to_owned()),
        "停止中の写真は再開後も送らない: {sent:?}"
    );
    assert!(sent.contains(&"4.png".to_owned()));
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_token_requires_relogin_and_never_falls_back_to_anonymous() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    login(&h).await;
    assert!(h.tokens.load().unwrap().is_some());
    h.wait_for("ログイン", |s| {
        matches!(s.account, AccountView::LoggedIn { .. })
    })
    .await;
    h.photo("ok.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    assert!(!h.snap().history[0].anonymous);

    mock.revoke_all();
    h.photo("after-revoke.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::AuthExpired))
        .await;
    let s = h
        .wait_for("要ログイン", |s| {
            matches!(s.account, AccountView::NeedsRelogin { .. })
        })
        .await;
    assert_eq!(s.tray, cq_core::engine::TrayState::NeedsLogin);
    // 次の写真は送らずに失敗として残す（匿名で送らない）
    h.photo("while-broken.png", 8, 8);
    h.wait_for("失敗として記録", |s| {
        s.history
            .iter()
            .any(|e| e.file_name == "while-broken.png" && e.status == EntryStatus::Failed)
            && s.history
                .iter()
                .any(|e| e.file_name == "after-revoke.png" && e.status == EntryStatus::Failed)
    })
    .await;
    let quick = mock.requests_to(Endpoint::Quick);
    assert!(
        quick.iter().all(|r| r.authorized),
        "匿名で送っていない: {quick:?}"
    );
    // 失効の検出が先に上限情報の取得（/me）で起きた場合、2 枚目は送らずに失敗になる。
    assert!((1..=2).contains(&quick.len()), "{quick:?}");
    let s = h.snap();
    let after = s
        .history
        .iter()
        .find(|e| e.file_name == "after-revoke.png")
        .unwrap();
    assert_eq!(after.status, EntryStatus::Failed);

    // 再ログインすると再試行できる
    login(&h).await;
    let id = h
        .snap()
        .history
        .iter()
        .find(|e| e.file_name == "while-broken.png")
        .unwrap()
        .id
        .clone();
    h.engine.retry(&id).unwrap();
    h.wait_for("再試行の完了", |s| {
        s.history
            .iter()
            .any(|e| e.id == id && e.status == EntryStatus::Done)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_errors_are_retried_then_persisted_as_failed() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    mock.push_error(Endpoint::Quick, 503, "SERVICE_UNAVAILABLE", json!({}));
    mock.push(
        Endpoint::Quick,
        Scripted::RateLimited {
            retry_after_seconds: 0,
        },
    );
    h.photo("flaky.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    assert_eq!(mock.requests_to(Endpoint::Quick).len(), 3);

    for _ in 0..3 {
        mock.push_error(Endpoint::Quick, 500, "INTERNAL", json!({}));
    }
    h.photo("down.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Failed { .. }))
        .await;
    let s = h.snap();
    let e = s
        .history
        .iter()
        .find(|e| e.file_name == "down.png")
        .unwrap()
        .clone();
    assert_eq!(e.status, EntryStatus::Failed);
    assert!(e.failure.as_ref().unwrap().retryable);
    assert_eq!(s.tray, cq_core::engine::TrayState::Error);
    h.engine.acknowledge_errors();
    assert_ne!(h.snap().tray, cq_core::engine::TrayState::Error);

    // 再起動後も失敗が残り、手動で再試行できる
    let h = h.restart(&mock.base_url()).await;
    let s = h.snap();
    let e2 = s.history.iter().find(|x| x.id == e.id).unwrap();
    assert_eq!(e2.status, EntryStatus::Failed);
    h.engine.retry(&e.id).unwrap();
    h.wait_for("再試行の完了", |s| {
        s.history
            .iter()
            .any(|x| x.id == e.id && x.status == EntryStatus::Done)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn timeouts_are_retried() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |s| {
        s.advanced.upload_timeout_secs = 1;
    })
    .await;
    mock.push(Endpoint::Quick, Scripted::Delay { ms: 2_500 });
    h.photo("slow.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    assert!(mock.requests_to(Endpoint::Quick).len() >= 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_photos_wait_and_upload_when_connection_returns() {
    let _slot = slot().await;
    // ポートを確保してから閉じる（接続できない状態）
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let url = format!("http://127.0.0.1:{port}");
    let mut h = Harness::new(&url, |_| {}).await;
    h.photo("o1.png", 8, 8);
    h.photo("o2.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Offline { .. }))
        .await;
    let s = h
        .wait_for("接続待ち", |s| {
            s.offline
                && s.history
                    .iter()
                    .filter(|e| e.status == EntryStatus::Waiting)
                    .count()
                    == 2
        })
        .await;
    assert_eq!(s.tray, cq_core::engine::TrayState::Offline);
    assert_eq!(
        s.history[0].display_status(chrono::Utc::now()),
        DisplayStatus::Waiting
    );

    let mock = MockServer::start_on(MockConfig::default(), port)
        .await
        .unwrap();
    h.next_event(|e| matches!(e, EngineEvent::BackOnline)).await;
    h.wait_for("自動で送信", |s| {
        !s.offline && s.history.iter().all(|e| e.status == EntryStatus::Done)
    })
    .await;
    assert_eq!(mock.requests_to(Endpoint::Quick).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn right_click_share_joins_urls_with_newlines() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |s| s.auto_upload = false).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let a = elsewhere.path().join("IMG_0412.png");
    let b = elsewhere.path().join("IMG_0413.png");
    let c = elsewhere.path().join("IMG_0414.png");
    for p in [&a, &b, &c] {
        write_png(p, 8, 8);
    }
    let bad = elsewhere.path().join("notes.txt");
    std::fs::write(&bad, b"x").unwrap();

    let id = h
        .engine
        .share_files(&[a.clone(), b.clone(), c.clone()], Some(Quality::Shrink));
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::BatchFinished { .. }))
        .await;
    assert!(matches!(
        ev,
        EngineEvent::BatchFinished {
            succeeded: 3,
            failed: 0,
            copied: true,
            ..
        }
    ));
    let view = h.engine.batch(&id).unwrap();
    assert!(view.finished);
    let clip = h.clipboard.get().unwrap();
    assert_eq!(clip.lines().count(), 3);
    assert_eq!(clip, view.urls.join("\n"));
    let names: Vec<_> = mock
        .requests_to(Endpoint::Quick)
        .into_iter()
        .filter_map(|r| r.file_name)
        .collect();
    assert_eq!(
        names,
        vec!["IMG_0412.png", "IMG_0413.png", "IMG_0414.png"],
        "選択順に送る"
    );
    assert!(
        mock.requests_to(Endpoint::Quick)
            .iter()
            .all(|r| r.quality.as_deref() == Some("shrink"))
    );

    // 一部失敗（対応していないファイル・存在しないファイル）
    let id = h.engine.share_files(
        &[a.clone(), bad, elsewhere.path().join("missing.png")],
        None,
    );
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::BatchFinished { .. }))
        .await;
    assert!(matches!(
        ev,
        EngineEvent::BatchFinished {
            succeeded: 1,
            failed: 2,
            ..
        }
    ));
    let view = h.engine.batch(&id).unwrap();
    assert!(
        matches!(view.items[1].status, BatchItemStatus::Failed(ref m) if m == "対応していないファイル形式です")
    );
    assert!(matches!(view.items[2].status, BatchItemStatus::Failed(_)));
    assert_eq!(h.clipboard.get().unwrap().lines().count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_and_remove_history_entries() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    h.photo("del.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    assert_eq!(mock.upload_count(), 1);
    let id = h.snap().history[0].id.clone();
    h.engine.delete_remote(&id).await.unwrap();
    assert_eq!(
        mock.upload_count(),
        0,
        "匿名の削除トークンでサーバーから消える"
    );
    let s = h
        .wait_for("削除済み", |s| {
            s.history[0].status == EntryStatus::Deleted
        })
        .await;
    assert!(s.history[0].delete_token.is_none());
    assert!(h.engine.copy_url(&id).is_err(), "削除済みはコピーできない");
    h.engine.remove_entry(&id).unwrap();
    assert!(h.snap().history.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn login_logout_and_auth_failures() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let h = Harness::new(&mock.base_url(), |_| {}).await;
    login(&h).await;
    assert_eq!(mock.token_count(), 1);
    let revoked = h.engine.logout().await.unwrap();
    assert!(revoked);
    assert_eq!(mock.token_count(), 0, "サーバー側で失効");
    assert!(
        h.tokens.load().unwrap().is_none(),
        "ローカルのトークンも削除"
    );
    assert_eq!(h.snap().account, AccountView::Anonymous);

    mock.configure(|c| c.authorize_mode = AuthorizeMode::Deny);
    let session = h.engine.start_login().await.unwrap();
    let url = session.browser_url().clone();
    let (res, ()) = tokio::join!(session.finish(), act_as_browser(&url));
    assert_eq!(res.unwrap_err().code(), "ACCESS_DENIED");

    // コード交換の失敗（モックが 400 を返す）
    mock.configure(|c| c.authorize_mode = AuthorizeMode::Approve);
    mock.push_error(Endpoint::Token, 400, "INVALID_REQUEST", json!({}));
    let session = h.engine.start_login().await.unwrap();
    let url = session.browser_url().clone();
    let (res, ()) = tokio::join!(session.finish(), act_as_browser(&url));
    assert_eq!(res.unwrap_err().code(), "INVALID_REQUEST");
    assert!(h.tokens.load().unwrap().is_none());

    // キャンセル
    let session = h.engine.start_login().await.unwrap();
    session.cancel_token().cancel();
    assert_eq!(session.finish().await.unwrap_err().code(), "AUTH_CANCELLED");
}

#[tokio::test(flavor = "multi_thread")]
async fn consecutive_auto_uploads_are_copied_together() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    h.photo("c1.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    h.photo("c2.png", 8, 8);
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    assert!(matches!(
        ev,
        EngineEvent::Uploaded {
            copied_count: 2,
            ..
        }
    ));
    assert_eq!(h.clipboard.get().unwrap().lines().count(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn uninstall_cleanup_revokes_and_deletes() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    login(&h).await;
    h.photo("u.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    assert_eq!(mock.upload_count(), 1);
    let report = h
        .engine
        .cleanup_for_uninstall(
            cq_core::engine::UninstallOptions {
                delete_local_data: true,
                unlink: true,
                delete_uploads: true,
            },
            |_| {},
        )
        .await;
    assert!(report.server_revoked);
    assert_eq!(report.uploads_not_deleted, 0);
    assert_eq!(mock.upload_count(), 0);
    assert_eq!(mock.token_count(), 0);
    assert!(h.tokens.load().unwrap().is_none());
    assert!(!h.root.join("history.json").exists());
    assert!(!h.root.join("settings.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn leftover_token_without_account_is_not_treated_as_anonymous() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let photos = dir.path().join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    // 実行状態（state.json）は無いが、資格情報マネージャーにはトークンが残っている状態
    let tokens = Arc::new(MemoryTokenStore::default());
    tokens
        .save(&cq_core::secret::SecretString::new(mock.issue_token()))
        .unwrap();
    let mut h = Harness::with_dirs(dir, root, photos, tokens, &mock.base_url(), |_| {}).await;
    h.wait_for("アカウントの復元", |s| {
        matches!(s.account, AccountView::LoggedIn { .. })
    })
    .await;
    h.photo("restored.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    let quick = mock.requests_to(Endpoint::Quick);
    assert_eq!(quick.len(), 1);
    assert!(quick[0].authorized, "トークンで送る（匿名にしない）");
}

#[tokio::test(flavor = "multi_thread")]
async fn photo_modified_after_upload_is_not_sent_again() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    let p = h.photo("edited.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    // 外部ツールがメタデータを書き足した（サイズと更新時刻が変わる）
    write_png(&p, 16, 16);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(mock.requests_to(Endpoint::Quick).len(), 1);
}

// ---- 契約 v1.1 ----

#[tokio::test(flavor = "multi_thread")]
async fn v11_forbidden_blocks_uploads_without_falling_back_to_anonymous() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    login(&h).await;
    mock.configure(|c| c.banned = true);
    h.photo("banned1.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::AccountBlocked))
        .await;
    let s = h.wait_for("利用停止", |s| s.blocked).await;
    assert_eq!(s.tray, cq_core::engine::TrayState::Blocked);
    assert_eq!(s.tray.label(), "利用不可");
    // 設定は ON のまま、表示は実際の状態（OFF）にする（Q-031）
    assert!(s.auto_upload_setting);
    assert!(!s.auto_upload);
    // 403 の失敗は再試行させない
    let failed = s
        .history
        .iter()
        .find(|e| e.file_name == "banned1.png")
        .unwrap();
    assert_eq!(failed.failure.as_ref().unwrap().code, "FORBIDDEN");
    assert!(!failed.failure.as_ref().unwrap().retryable);
    assert!(h.engine.retry(&failed.id).is_err());
    // 再ログインを求めない（BAN は再認可で解決しない）
    assert!(matches!(s.account, AccountView::LoggedIn { .. }));
    h.photo("banned2.png", 8, 8);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let quick = mock.requests_to(Endpoint::Quick);
    assert!(quick.iter().all(|r| r.authorized), "匿名で送っていない");
    let names: Vec<_> = quick.iter().filter_map(|r| r.file_name.clone()).collect();
    assert!(
        !names.contains(&"banned2.png".to_owned()),
        "停止後の写真は送らない: {names:?}"
    );
    // 解除されたら上限情報の取得で戻る
    mock.configure(|c| c.banned = false);
    h.engine.refresh_limits().await;
    let s = h.snap();
    assert!(!s.blocked);
    assert!(s.auto_upload, "解除されたら元の設定（ON）に戻る");
    assert_ne!(s.tray, cq_core::engine::TrayState::Blocked);
}

#[tokio::test(flavor = "multi_thread")]
async fn v11_html_413_and_pixel_limit_are_shrunk_and_resent() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    mock.push(
        Endpoint::Quick,
        Scripted::Raw {
            status: 413,
            content_type: "text/html".into(),
            body: "<html><head><title>413 Request Entity Too Large</title></head><body>nginx</body></html>".into(),
        },
    );
    h.photo("nginx.png", 2200, 1100);
    h.next_event(|e| matches!(e, EngineEvent::Shrunk { .. }))
        .await;
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    mock.push_error(
        Endpoint::Quick,
        413,
        "SIZE_LIMIT_EXCEEDED",
        json!({ "limitBytes": 52428800, "kind": "IMAGE", "limitPixels": 100000000 }),
    );
    h.photo("pixels.png", 2200, 1100);
    h.next_event(|e| matches!(e, EngineEvent::Shrunk { .. }))
        .await;
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    let reqs = mock.requests_to(Endpoint::Quick);
    assert_eq!(reqs.len(), 4);
    assert_eq!(reqs[1].quality.as_deref(), Some("shrink"));
    assert_eq!(reqs[3].quality.as_deref(), Some("shrink"));
}

#[tokio::test(flavor = "multi_thread")]
async fn v11_daily_limit_details_are_used() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    mock.push_error(
        Endpoint::Quick,
        429,
        "DAILY_LIMIT_EXCEEDED",
        json!({ "resetsAt": "2099-01-01T15:00:00Z", "dailyLimit": 7 }),
    );
    h.photo("d.png", 8, 8);
    let ev = h
        .next_event(|e| matches!(e, EngineEvent::DailyLimitReached { .. }))
        .await;
    assert!(matches!(
        ev,
        EngineEvent::DailyLimitReached { limit: Some(7), .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn v11_token_format_issuance_pause_and_ban_at_login() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let mut h = Harness::new(&mock.base_url(), |_| {}).await;
    // 許可のリダイレクトは 303、URL には device_platform が付く
    let session = h.engine.start_login().await.unwrap();
    let url = session.browser_url().clone();
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "device_platform" && v.starts_with("Windows"))
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = client.get(url.as_str()).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 303);
    let loc = r
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let (res, cb) = tokio::join!(session.finish(), client.get(loc).send());
    res.unwrap();
    // ループバックは Clipshare の完了画面へ戻す
    let cb = cb.unwrap();
    assert_eq!(cb.status().as_u16(), 302);
    assert!(
        cb.headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("/qick/authorize/complete")
    );
    let t = h.tokens.load().unwrap().unwrap();
    assert!(t.expose().starts_with("csq_") && t.expose().len() == 47);

    // トークンありのアップロードには deleteToken が無い
    h.photo("authed.png", 8, 8);
    h.next_event(|e| matches!(e, EngineEvent::Uploaded { .. }))
        .await;
    let s = h
        .wait_for("完了", |s| {
            s.history
                .first()
                .is_some_and(|e| e.status == EntryStatus::Done)
        })
        .await;
    assert!(s.history[0].delete_token.is_none());
    assert!(!s.history[0].anonymous);

    h.engine.logout().await.unwrap();
    mock.configure(|c| c.token_issuance_paused = true);
    let session = h.engine.start_login().await.unwrap();
    let url = session.browser_url().clone();
    let (res, ()) = tokio::join!(session.finish(), act_as_browser(&url));
    assert_eq!(res.unwrap_err().code(), "SERVICE_UNAVAILABLE");

    mock.configure(|c| {
        c.token_issuance_paused = false;
        c.banned = true;
    });
    let session = h.engine.start_login().await.unwrap();
    let url = session.browser_url().clone();
    let (res, ()) = tokio::join!(session.finish(), act_as_browser(&url));
    assert_eq!(res.unwrap_err().code(), "FORBIDDEN");
    assert!(h.tokens.load().unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn v11_mock_rejects_nameless_part_and_mismatched_type() {
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let client = reqwest::Client::new();
    let png = {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        write_png(&p, 4, 4);
        std::fs::read(&p).unwrap()
    };
    let url = format!("{}/api/v1/quick", mock.base_url());
    let nameless =
        reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(png.clone()));
    let r = client.post(&url).multipart(nameless).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 400);
    let mismatched = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(png).file_name("clip.mp4"),
    );
    let r = client
        .post(&url)
        .multipart(mismatched)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 415);
}

// ---- 契約 v1.2: client/latest の sha256 ----

#[tokio::test(flavor = "multi_thread")]
async fn v12_update_sha256_present_null_and_mismatch() {
    use cq_core::update::{UpdateDecision, decide, verify_download};
    use sha2::Digest;
    let _slot = slot().await;
    let mock = MockServer::start(MockConfig::default()).await.unwrap();
    let h = Harness::new(&mock.base_url(), |_| {}).await;
    let body = b"fake msix bytes".to_vec();
    let good = cq_core::api::hex(&sha2::Sha256::digest(&body));
    let url = format!("{}/download/ClipshareQuick-9.0.0.msix", mock.base_url());
    let latest = |sha: serde_json::Value| {
        json!({
            "version": "9.0.0", "publishedAt": "2026-11-21T00:00:00Z", "notes": "n", "minSupportedVersion": null,
            "assets": [{ "name": "ClipshareQuick-9.0.0.msix", "url": url, "sizeBytes": body.len(), "sha256": sha }]
        })
    };
    let api = h.engine.api();
    let dir = tempfile::tempdir().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();

    // 1) sha256 あり・一致 → 検証に通る
    mock.configure(|c| {
        c.latest = Some(latest(json!(good)));
        c.download_body = body.clone();
    });
    let l = api.latest_client().await.unwrap();
    let UpdateDecision::Available { asset, .. } = decide("0.1.0", &l).unwrap() else {
        panic!("自動更新の対象になるはず")
    };
    let dest = dir.path().join("ok.msix");
    let sha = api
        .download_to(&asset.url, &dest, asset.size_bytes + 1024, None, &cancel)
        .await
        .unwrap();
    assert!(verify_download(&asset, &sha, std::fs::metadata(&dest).unwrap().len()).is_ok());

    // 2) sha256 が不一致 → 検証で弾く（適用しない）
    mock.configure(|c| c.latest = Some(latest(json!("0".repeat(64)))));
    let l = api.latest_client().await.unwrap();
    let UpdateDecision::Available { asset, .. } = decide("0.1.0", &l).unwrap() else {
        panic!()
    };
    let dest = dir.path().join("bad.msix");
    let sha = api
        .download_to(&asset.url, &dest, asset.size_bytes + 1024, None, &cancel)
        .await
        .unwrap();
    assert!(verify_download(&asset, &sha, std::fs::metadata(&dest).unwrap().len()).is_err());

    // 3) sha256 が null → 自動インストールせず、ダウンロードページへ案内
    mock.configure(|c| c.latest = Some(latest(serde_json::Value::Null)));
    let l = api.latest_client().await.unwrap();
    assert!(matches!(
        decide("0.1.0", &l).unwrap(),
        UpdateDecision::ManualOnly { .. }
    ));

    // 未取得時は 503
    mock.configure(|c| c.latest = None);
    assert!(api.latest_client().await.is_err());
}
