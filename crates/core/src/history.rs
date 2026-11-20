//! 履歴（直近 N 件、永続化）と処理済みファイルの記録（重複送信の防止）。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::fsutil;
use crate::media::MediaKind;
use crate::secret::SecretString;
use crate::settings::Quality;

/// 保存している状態。期限切れは保存せず、表示時に [`HistoryEntry::display_status`] で判定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryStatus {
    /// 送信待ち（キューに入っている）。
    Queued,
    Uploading,
    /// 動画の変換中（URLは使える）。
    Processing,
    Done,
    /// 接続待ち（オフライン）。接続が戻ると自動で送る。
    Waiting,
    Failed,
    /// サーバーから削除した。
    Deleted,
}

/// 画面に出す状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStatus {
    Queued,
    Uploading,
    Processing,
    Done,
    Waiting,
    Failed,
    Expired,
    Deleted,
}

impl DisplayStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "待機中",
            Self::Uploading => "アップロード中",
            Self::Processing => "処理中",
            Self::Done => "完了",
            Self::Waiting => "待機中",
            Self::Failed => "失敗",
            Self::Expired => "期限切れ",
            Self::Deleted => "削除済み",
        }
    }
}

/// 送信のきっかけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntrySource {
    /// フォルダ監視による自動アップロード。
    Auto,
    /// 右クリックなど手動の共有。
    Manual,
}

/// 失敗の内容（画面表示用の日本語と、ログ・診断用のコード）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
    /// 手動で再試行できるか。
    pub retryable: bool,
}

/// 履歴の 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub file_name: String,
    pub path: PathBuf,
    pub kind: MediaKind,
    pub quality: Quality,
    pub source: EntrySource,
    pub status: EntryStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub remote_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// 匿名アップロードの削除用トークン（API トークンではない）。
    #[serde(default)]
    pub delete_token: Option<SecretString>,
    #[serde(default)]
    pub anonymous: bool,
    /// サイズ超過で自動縮小して送ったか。
    #[serde(default)]
    pub shrunk: bool,
    #[serde(default)]
    pub failure: Option<Failure>,
    /// 右クリック共有のまとまり（進捗ウィンドウ用）。
    #[serde(default)]
    pub batch_id: Option<String>,
}

impl HistoryEntry {
    pub fn new(
        path: &Path,
        kind: MediaKind,
        quality: Quality,
        source: EntrySource,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: new_id(),
            file_name: crate::logging::file_name_of(path),
            path: path.to_path_buf(),
            kind,
            quality,
            source,
            status: EntryStatus::Queued,
            created_at: now,
            size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            remote_id: None,
            url: None,
            expires_at: None,
            delete_token: None,
            anonymous: false,
            shrunk: false,
            failure: None,
            batch_id: None,
        }
    }

    /// 期限切れを考慮した表示用の状態。
    pub fn display_status(&self, now: DateTime<Utc>) -> DisplayStatus {
        let expired = self.expires_at.is_some_and(|e| e <= now);
        match self.status {
            EntryStatus::Done | EntryStatus::Processing if expired => DisplayStatus::Expired,
            EntryStatus::Queued => DisplayStatus::Queued,
            EntryStatus::Uploading => DisplayStatus::Uploading,
            EntryStatus::Processing => DisplayStatus::Processing,
            EntryStatus::Done => DisplayStatus::Done,
            EntryStatus::Waiting => DisplayStatus::Waiting,
            EntryStatus::Failed => DisplayStatus::Failed,
            EntryStatus::Deleted => DisplayStatus::Deleted,
        }
    }

    /// 送信中または送信待ち（終了時に失われてはいけないもの）。
    pub fn is_pending(&self) -> bool {
        matches!(
            self.status,
            EntryStatus::Queued | EntryStatus::Uploading | EntryStatus::Waiting
        )
    }

    /// URL をまだ使えるか。
    pub fn url_usable(&self, now: DateTime<Utc>) -> bool {
        self.url.is_some()
            && matches!(
                self.display_status(now),
                DisplayStatus::Done | DisplayStatus::Processing
            )
    }

    /// 「残り 23時間」のような残り時間。
    pub fn remaining_label(&self, now: DateTime<Utc>) -> Option<String> {
        let left = self.expires_at? - now;
        if left <= chrono::TimeDelta::zero() {
            return None;
        }
        let hours = left.num_hours();
        Some(if hours >= 1 {
            format!("残り {hours}時間")
        } else {
            format!("残り {}分", left.num_minutes().max(1))
        })
    }
}

/// 推測しにくいローカル ID。
pub fn new_id() -> String {
    let mut b = [0u8; 8];
    rand::Rng::fill(&mut rand::rng(), &mut b[..]);
    crate::api::hex(&b)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HistoryFile {
    entries: Vec<HistoryEntry>,
}

/// 履歴の保存先。新しい順に並べる。
#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    limit: usize,
    entries: Vec<HistoryEntry>,
}

impl HistoryStore {
    /// 読み込む。前回終了時に送信中だったものは「送信待ち」に戻す（再起動後に送り直す）。
    pub fn load(path: &Path, limit: usize) -> Self {
        let mut entries = match fsutil::load_json::<HistoryFile>(path) {
            Ok(Some(f)) => f.entries,
            Ok(None) => Vec::new(),
            Err(e) => {
                tracing::warn!(error = %e, "履歴を読み込めないため空で始めます");
                fsutil::quarantine(path);
                Vec::new()
            }
        };
        for e in &mut entries {
            if e.status == EntryStatus::Uploading {
                e.status = EntryStatus::Queued;
            }
        }
        let mut s = Self {
            path: path.to_path_buf(),
            limit: limit.max(1),
            entries,
        };
        s.trim();
        s
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        self.trim();
    }

    pub fn save(&self) -> Result<(), StorageError> {
        fsutil::save_json(
            &self.path,
            &HistoryFile {
                entries: self.entries.clone(),
            },
        )
    }

    /// 上限を超えた分を古い順に消す。送信待ちのものは消さない。
    fn trim(&mut self) {
        while self.entries.len() > self.limit {
            match self.entries.iter().rposition(|e| !e.is_pending()) {
                Some(i) => {
                    self.entries.remove(i);
                }
                None => break,
            }
        }
    }

    /// 先頭（最新）に追加する。
    pub fn add(&mut self, entry: HistoryEntry) {
        self.entries.insert(0, entry);
        self.trim();
    }

    pub fn get(&self, id: &str) -> Option<&HistoryEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// 1 件を書き換える。見つからなければ `false`。
    pub fn update<F: FnOnce(&mut HistoryEntry)>(&mut self, id: &str, f: F) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                f(e);
                true
            }
            None => false,
        }
    }

    /// 履歴から消す（サーバーの削除はしない）。
    pub fn remove(&mut self, id: &str) -> Option<HistoryEntry> {
        let i = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(i))
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// 送信待ち（古い順）。
    pub fn pending_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .rev()
            .filter(|e| matches!(e.status, EntryStatus::Queued | EntryStatus::Waiting))
            .map(|e| e.id.clone())
            .collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// ファイルの同一性（パス + サイズ + 更新時刻）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint {
    pub path: String,
    pub size: u64,
    pub modified_ns: u128,
}

impl Fingerprint {
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let m = std::fs::metadata(path)?;
        let modified_ns = m
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(Self {
            path: crate::settings::folder_key(path),
            size: m.len(),
            modified_ns,
        })
    }
}

/// 処理済み（送った・送らないと決めた）ファイルの記録。履歴より長く保持する。
#[derive(Debug)]
pub struct ProcessedSet {
    path: PathBuf,
    cap: usize,
    items: VecDeque<Fingerprint>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProcessedFile {
    items: Vec<Fingerprint>,
}

impl ProcessedSet {
    pub const DEFAULT_CAP: usize = 5_000;

    pub fn load(path: &Path, cap: usize) -> Self {
        let items = match fsutil::load_json::<ProcessedFile>(path) {
            Ok(Some(f)) => f.items.into(),
            Ok(None) => VecDeque::new(),
            Err(e) => {
                tracing::warn!(error = %e, "処理済みの記録を読み込めないため空で始めます");
                fsutil::quarantine(path);
                VecDeque::new()
            }
        };
        Self {
            path: path.to_path_buf(),
            cap: cap.max(1),
            items,
        }
    }

    /// 同じパスのファイルを処理したことがあるか（内容が後から変わっても二重に送らないため）。
    pub fn contains_path(&self, path_key: &str) -> bool {
        self.items.iter().any(|f| f.path == path_key)
    }

    pub fn contains(&self, fp: &Fingerprint) -> bool {
        self.items.contains(fp)
    }

    /// 追加する。新規なら `true`。
    pub fn insert(&mut self, fp: Fingerprint) -> bool {
        if self.contains(&fp) {
            return false;
        }
        self.items.push_back(fp);
        while self.items.len() > self.cap {
            self.items.pop_front();
        }
        true
    }

    pub fn save(&self) -> Result<(), StorageError> {
        fsutil::save_json(
            &self.path,
            &ProcessedFile {
                items: self.items.iter().cloned().collect(),
            },
        )
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn entry(name: &str, status: EntryStatus) -> HistoryEntry {
        let mut e = HistoryEntry::new(
            Path::new(&format!(r"C:\p\{name}")),
            MediaKind::Image,
            Quality::Original,
            EntrySource::Auto,
            Utc::now(),
        );
        e.status = status;
        e
    }

    #[test]
    fn keeps_latest_n_and_never_drops_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = HistoryStore::load(&dir.path().join("h.json"), 3);
        h.add(entry("pending.png", EntryStatus::Waiting));
        for i in 0..5 {
            h.add(entry(&format!("{i}.png"), EntryStatus::Done));
        }
        let names: Vec<_> = h.entries().iter().map(|e| e.file_name.as_str()).collect();
        assert_eq!(names, vec!["4.png", "3.png", "pending.png"]);
        assert_eq!(h.entries().len(), 3);
    }

    #[test]
    fn fifty_is_the_default_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = HistoryStore::load(&dir.path().join("h.json"), 50);
        for i in 0..60 {
            h.add(entry(&format!("{i}.png"), EntryStatus::Done));
        }
        assert_eq!(h.entries().len(), 50);
        assert_eq!(h.entries()[0].file_name, "59.png");
    }

    #[test]
    fn expired_is_displayed_and_kept() {
        let now = Utc::now();
        let mut e = entry("a.png", EntryStatus::Done);
        e.url = Some("https://clipshare.link/q/x".into());
        e.expires_at = Some(now - TimeDelta::minutes(1));
        assert_eq!(e.display_status(now), DisplayStatus::Expired);
        assert!(!e.url_usable(now));
        assert_eq!(e.display_status(now).label(), "期限切れ");
        e.expires_at = Some(now + TimeDelta::hours(23) + TimeDelta::minutes(5));
        assert_eq!(e.display_status(now), DisplayStatus::Done);
        assert_eq!(e.remaining_label(now).as_deref(), Some("残り 23時間"));
        e.expires_at = Some(now + TimeDelta::minutes(5));
        assert_eq!(e.remaining_label(now).as_deref(), Some("残り 5分"));
    }

    #[test]
    fn persists_and_resets_uploading_to_queued() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.json");
        let mut h = HistoryStore::load(&p, 50);
        let e = entry("a.png", EntryStatus::Uploading);
        let id = e.id.clone();
        h.add(e);
        h.add(entry("b.png", EntryStatus::Failed));
        h.save().unwrap();
        let h2 = HistoryStore::load(&p, 50);
        assert_eq!(h2.get(&id).unwrap().status, EntryStatus::Queued);
        assert_eq!(h2.pending_ids(), vec![id.clone()]);
        let mut h2 = h2;
        assert!(h2.update(&id, |e| e.status = EntryStatus::Done));
        assert!(h2.remove(&id).is_some());
        assert!(h2.get(&id).is_none());
    }

    #[test]
    fn processed_set_dedupes_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("p.json");
        let mut s = ProcessedSet::load(&p, 2);
        let fp = |n: u64| Fingerprint {
            path: "a".into(),
            size: n,
            modified_ns: 1,
        };
        assert!(s.insert(fp(1)));
        assert!(!s.insert(fp(1)));
        assert!(s.insert(fp(2)));
        assert!(s.insert(fp(3)));
        assert!(!s.contains(&fp(1)), "古いものから消える");
        s.save().unwrap();
        let s2 = ProcessedSet::load(&p, 2);
        assert!(s2.contains(&fp(3)));
    }

    #[test]
    fn fingerprint_changes_when_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        std::fs::write(&p, b"1").unwrap();
        let a = Fingerprint::of(&p).unwrap();
        std::fs::write(&p, b"22").unwrap();
        let b = Fingerprint::of(&p).unwrap();
        assert_ne!(a, b);
    }
}
