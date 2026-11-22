//! 写真フォルダの監視。
//!
//! - サブフォルダ（`YYYY-MM`）も含めて監視する。
//! - 書き込み途中のファイルを拾わない: サイズと更新時刻が一定時間変わらず、かつ排他で開けるまで待つ。
//!   最大待ち時間を超えたら諦める。
//! - 監視開始より前に作られたファイルは無視する（初回起動時の過去写真・既存写真の更新）。
//! - 同じファイルの重複イベントは 1 回にまとめる（処理中の集合）。送信済みかどうかの判定は呼び出し側
//!   （[`crate::history::ProcessedSet`]）で行う。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::logging::mask_path;
use crate::media::MediaKind;
use crate::settings::StabilitySettings;

/// 監視の設定。
#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub folders: Vec<PathBuf>,
    pub include_videos: bool,
    pub stability: StabilitySettings,
    /// これより前に作られたファイルは無視する。
    pub started_at: SystemTime,
}

/// 書き込みが終わったファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyFile {
    pub path: PathBuf,
    pub kind: MediaKind,
}

/// フォルダごとの監視状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderStatus {
    pub folder: PathBuf,
    pub watching: bool,
}

/// 監視中のハンドル。ドロップすると監視を止める。
#[derive(Debug)]
pub struct FolderWatcher {
    _watchers: Vec<notify::RecommendedWatcher>,
    /// 通知が 1 つも来ない（フォルダが無い）場合も再走査を続けられるよう、送信側を保持する。
    _raw_tx: mpsc::UnboundedSender<PathBuf>,
    status: Vec<FolderStatus>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

impl FolderWatcher {
    /// 監視を始める。存在しないフォルダは監視せず、状態に `watching: false` で残す。
    pub fn start(opts: WatchOptions, out: mpsc::UnboundedSender<ReadyFile>) -> Self {
        let (raw_tx, raw_rx) = mpsc::unbounded_channel::<PathBuf>();
        let mut watchers = Vec::new();
        let mut status = Vec::new();
        for folder in &opts.folders {
            let tx = raw_tx.clone();
            let handler = move |res: notify::Result<notify::Event>| match res {
                Ok(ev) => {
                    if is_interesting(&ev.kind) {
                        for p in ev.paths {
                            let _ = tx.send(p);
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "フォルダ監視でエラーが発生しました"),
            };
            let started = notify::recommended_watcher(handler).and_then(|mut w| {
                w.watch(folder, RecursiveMode::Recursive)?;
                Ok(w)
            });
            match started {
                Ok(w) => {
                    tracing::info!(folder = %mask_path(folder), "フォルダの監視を開始しました");
                    watchers.push(w);
                    status.push(FolderStatus {
                        folder: folder.clone(),
                        watching: true,
                    });
                }
                Err(e) => {
                    tracing::warn!(folder = %mask_path(folder), error = %e, "フォルダを監視できません");
                    status.push(FolderStatus {
                        folder: folder.clone(),
                        watching: false,
                    });
                }
            }
        }
        let cancel = CancellationToken::new();
        let task = tokio::spawn(process(opts, raw_rx, out, cancel.clone()));
        Self {
            _watchers: watchers,
            _raw_tx: raw_tx,
            status,
            cancel,
            task,
        }
    }

    pub fn status(&self) -> &[FolderStatus] {
        &self.status
    }
}

fn is_interesting(kind: &EventKind) -> bool {
    use notify::event::{ModifyKind, RenameMode};
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(
                ModifyKind::Data(_)
                    | ModifyKind::Any
                    | ModifyKind::Metadata(_)
                    | ModifyKind::Name(RenameMode::To | RenameMode::Both | RenameMode::Any)
            )
    )
}

/// 監視対象の種別か。
fn kind_of(path: &Path, include_videos: bool) -> Option<MediaKind> {
    match MediaKind::from_path(path)? {
        MediaKind::Video if !include_videos => None,
        k => Some(k),
    }
}

/// 書き込み完了を送ったときのファイルの状態（同じ状態なら再走査で送り直さない）。
type Signature = (u64, Option<SystemTime>);

fn signature(path: &Path) -> Option<Signature> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

/// 監視フォルダを走査し、監視開始後に作られた対象ファイルを返す（通知の取りこぼし対策）。
/// 走査はフォルダ直下と 1 階層下（年月のサブフォルダ）だけ。監視開始後に更新されていないサブフォルダは見ない。
pub fn scan_new_files(
    folders: &[PathBuf],
    started_at: SystemTime,
    include_videos: bool,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let recent = |m: &std::fs::Metadata| {
        m.modified()
            .is_ok_and(|t| t + Duration::from_secs(1) >= started_at)
    };
    let visit = |dir: &Path, top: bool, out: &mut Vec<PathBuf>, subdirs: &mut Vec<PathBuf>| {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() && top {
                if e.metadata().is_ok_and(|m| recent(&m)) {
                    subdirs.push(p);
                }
            } else if ft.is_file()
                && kind_of(&p, include_videos).is_some()
                && let Ok(m) = e.metadata()
                && !created_before(&m, started_at)
            {
                out.push(p);
            }
        }
    };
    for f in folders {
        let mut subdirs = Vec::new();
        visit(f, true, &mut out, &mut subdirs);
        for d in subdirs {
            visit(&d, false, &mut out, &mut Vec::new());
        }
    }
    out
}

async fn process(
    opts: WatchOptions,
    mut raw_rx: mpsc::UnboundedReceiver<PathBuf>,
    out: mpsc::UnboundedSender<ReadyFile>,
    cancel: CancellationToken,
) {
    let mut in_flight: HashSet<PathBuf> = HashSet::new();
    let mut emitted: std::collections::HashMap<PathBuf, Option<Signature>> =
        std::collections::HashMap::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(PathBuf, bool)>();
    let rescan_ms = opts.stability.rescan_interval_ms;
    let mut rescan = tokio::time::interval(Duration::from_millis(rescan_ms.max(100)));
    rescan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let candidates: Vec<PathBuf> = tokio::select! {
            () = cancel.cancelled() => break,
            Some((done, ready)) = done_rx.recv() => {
                in_flight.remove(&done);
                if ready {
                    let sig = signature(&done);
                    if emitted.len() >= 2_000 {
                        emitted.clear();
                    }
                    emitted.insert(done, sig);
                }
                continue;
            }
            _ = rescan.tick(), if rescan_ms > 0 => {
                let folders = opts.folders.clone();
                let (started, videos) = (opts.started_at, opts.include_videos);
                let found = tokio::task::spawn_blocking(move || scan_new_files(&folders, started, videos))
                    .await
                    .unwrap_or_default();
                found
                    .into_iter()
                    .filter(|p| emitted.get(p).is_none_or(|sig| *sig != signature(p)))
                    .collect()
            }
            next = raw_rx.recv() => match next {
                Some(p) => vec![p],
                None => break,
            },
        };
        for path in candidates {
            tracing::debug!(file = %crate::logging::file_name_of(&path), "監視: 候補を受け取りました");
            let Some(kind) = kind_of(&path, opts.include_videos) else {
                continue;
            };
            if !in_flight.insert(path.clone()) {
                continue;
            }
            let stability = opts.stability.clone();
            let started_at = opts.started_at;
            let out = out.clone();
            let done_tx = done_tx.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let ready = tokio::select! {
                    r = settle(&path, &stability, started_at) => r,
                    () = cancel.cancelled() => false,
                };
                if ready {
                    let _ = out.send(ReadyFile {
                        path: path.clone(),
                        kind,
                    });
                }
                let _ = done_tx.send((path, ready));
            });
        }
    }
}

/// 監視開始より前に作られたか。
fn created_before(meta: &std::fs::Metadata, started_at: SystemTime) -> bool {
    let created = meta.created().or_else(|_| meta.modified());
    match created {
        // ファイルシステムの時刻の粒度を考慮して 1 秒の余裕を持たせる。
        Ok(t) => t + Duration::from_secs(1) < started_at,
        Err(_) => false,
    }
}

/// 書き込みが終わるまで待つ。送ってよいなら `true`。
async fn settle(path: &Path, s: &StabilitySettings, started_at: SystemTime) -> bool {
    let poll = Duration::from_millis(s.poll_interval_ms);
    let stable_for = Duration::from_millis(s.stable_for_ms);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(s.max_wait_ms);
    let mut last: Option<(u64, Option<SystemTime>)> = None;
    let mut stable_since = tokio::time::Instant::now();
    loop {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => {
                if created_before(&meta, started_at) {
                    tracing::debug!(file = %crate::logging::file_name_of(path), "監視開始より前のファイルのため無視します");
                    return false;
                }
                let sig = (meta.len(), meta.modified().ok());
                if last.as_ref() != Some(&sig) || sig.0 == 0 {
                    last = Some(sig);
                    stable_since = tokio::time::Instant::now();
                } else if stable_since.elapsed() >= stable_for && can_open_exclusively(path) {
                    return true;
                }
            }
            Ok(_) => return false,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 一時ファイルの削除・名前変更など
                return false;
            }
            Err(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(file = %crate::logging::file_name_of(path), "書き込みが終わらないため送信を見送りました");
            return false;
        }
        tokio::time::sleep(poll).await;
    }
}

/// 書き込み中のプロセスがいないか。
///
/// 読み取りの共有だけを許して開く。書き込み用に開いているプロセス（VRChat が保存中など）があれば共有違反で失敗し、
/// 読み取りだけのプロセス（ウイルス対策ソフトのスキャンなど）は妨げにならない。
#[cfg(windows)]
fn can_open_exclusively(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .is_ok()
}

#[cfg(not(windows))]
fn can_open_exclusively(path: &Path) -> bool {
    std::fs::File::open(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fast() -> StabilitySettings {
        StabilitySettings {
            poll_interval_ms: 20,
            stable_for_ms: 150,
            max_wait_ms: 5_000,
            rescan_interval_ms: 0,
        }
    }

    async fn recv_within(
        rx: &mut mpsc::UnboundedReceiver<ReadyFile>,
        ms: u64,
    ) -> Option<ReadyFile> {
        tokio::time::timeout(Duration::from_millis(ms), rx.recv())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ignores_files_created_before_watch_started() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.png");
        std::fs::write(&old, b"old").unwrap();
        // 監視開始時刻を既存ファイルの作成時刻より十分後ろにする
        let started_at = SystemTime::now() + Duration::from_secs(2);
        let (tx, mut rx2) = mpsc::unbounded_channel();
        let w = FolderWatcher::start(
            WatchOptions {
                folders: vec![dir.path().to_path_buf()],
                include_videos: false,
                stability: fast(),
                started_at,
            },
            tx,
        );
        assert!(w.status()[0].watching);
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(&old, b"old-modified").unwrap();
        assert!(recv_within(&mut rx2, 800).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn detects_new_file_in_subfolder_only_after_writing_completes() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _w = FolderWatcher::start(
            WatchOptions {
                folders: vec![dir.path().to_path_buf()],
                include_videos: false,
                stability: fast(),
                started_at: SystemTime::now() - Duration::from_secs(5),
            },
            tx,
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let sub = dir.path().join("2026-11");
        std::fs::create_dir(&sub).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let p = sub.join("VRChat_2026-11-19_21-04-12.png");
        let mut f = std::fs::File::create(&p).unwrap();
        for _ in 0..5 {
            f.write_all(&[0u8; 1024]).unwrap();
            f.flush().unwrap();
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        // 開いたままの間は送らない
        assert!(recv_within(&mut rx, 300).await.is_none());
        drop(f);
        let got = recv_within(&mut rx, 3_000)
            .await
            .expect("書き込み完了後に検出される");
        assert_eq!(got.path, p);
        assert_eq!(got.kind, MediaKind::Image);
        // 同じファイルの重複イベントで 2 回目は出ない
        assert!(recv_within(&mut rx, 500).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn videos_only_when_enabled_and_other_files_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _w = FolderWatcher::start(
            WatchOptions {
                folders: vec![dir.path().to_path_buf()],
                include_videos: false,
                stability: fast(),
                started_at: SystemTime::now() - Duration::from_secs(5),
            },
            tx,
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(dir.path().join("clip.mp4"), b"v").unwrap();
        std::fs::write(dir.path().join("note.txt"), b"t").unwrap();
        assert!(recv_within(&mut rx, 700).await.is_none());

        let (tx, mut rx) = mpsc::unbounded_channel();
        let _w2 = FolderWatcher::start(
            WatchOptions {
                folders: vec![dir.path().to_path_buf()],
                include_videos: true,
                stability: fast(),
                started_at: SystemTime::now() - Duration::from_secs(5),
            },
            tx,
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(dir.path().join("clip2.mp4"), b"v").unwrap();
        let got = recv_within(&mut rx, 3_000).await.unwrap();
        assert_eq!(got.kind, MediaKind::Video);
    }

    #[test]
    fn rescan_finds_only_files_created_after_start() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.png"), b"x").unwrap();
        let started = SystemTime::now() + Duration::from_secs(2);
        assert!(scan_new_files(&[dir.path().to_path_buf()], started, false).is_empty());
        let started = SystemTime::now() - Duration::from_secs(5);
        let sub = dir.path().join("2026-11");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("new.png"), b"x").unwrap();
        std::fs::write(sub.join("clip.mp4"), b"x").unwrap();
        let found = scan_new_files(&[dir.path().to_path_buf()], started, false);
        assert!(found.iter().any(|p| p.ends_with("new.png")));
        assert!(!found.iter().any(|p| p.ends_with("clip.mp4")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rescan_catches_files_when_notifications_are_missed() {
        // 存在しないフォルダを監視して通知を受けられない状態にし、後からフォルダを作る
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("late");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut st = fast();
        st.rescan_interval_ms = 200;
        let _w = FolderWatcher::start(
            WatchOptions {
                folders: vec![folder.clone()],
                include_videos: false,
                stability: st,
                started_at: SystemTime::now() - Duration::from_secs(5),
            },
            tx,
        );
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("a.png"), b"x").unwrap();
        let got = recv_within(&mut rx, 5_000).await.expect("再走査で見つかる");
        assert!(got.path.ends_with("a.png"));
        // 同じ状態のファイルは再走査で何度も送らない
        assert!(recv_within(&mut rx, 800).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_folder_is_reported_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let (tx, _rx) = mpsc::unbounded_channel();
        let w = FolderWatcher::start(
            WatchOptions {
                folders: vec![missing.clone()],
                include_videos: false,
                stability: fast(),
                started_at: SystemTime::now(),
            },
            tx,
        );
        assert!(!w.status()[0].watching);
        assert!(!missing.exists());
    }
}
