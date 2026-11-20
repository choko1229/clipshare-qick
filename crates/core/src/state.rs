//! 実行状態（連携中のアカウント、1日上限による停止、更新確認の時刻）の永続化。
//!
//! トークンそのものはここに置かない（資格情報マネージャーのみ）。

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::api::User;
use crate::error::StorageError;
use crate::fsutil;

/// 連携中のアカウント（表示用の情報のみ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub user: User,
    pub token_id: String,
    pub linked_at: DateTime<Utc>,
}

/// 1日上限による自動アップロードの停止。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DailyPause {
    /// API が返したリセット時刻。
    pub until: Option<DateTime<Utc>>,
    /// リセット時刻が無いときは、この現地日付のあいだ停止する。
    pub local_date: Option<NaiveDate>,
}

impl DailyPause {
    pub fn start(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        match resets_at {
            Some(t) if t > now => Self {
                until: Some(t),
                local_date: None,
            },
            _ => Self {
                until: None,
                local_date: Some(now.with_timezone(&Local).date_naive()),
            },
        }
    }

    /// 停止中か（リセット時刻を過ぎた、または現地の日付が変わったら再開）。
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        if let Some(t) = self.until {
            return now < t;
        }
        if let Some(d) = self.local_date {
            return now.with_timezone(&Local).date_naive() == d;
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PersistentState {
    pub account: Option<Account>,
    pub daily_pause: Option<DailyPause>,
    /// 401 を受けて再ログインが必要な状態。
    pub needs_relogin: bool,
    pub last_update_check: Option<DateTime<Utc>>,
}

/// 実行状態の保存先。
#[derive(Debug)]
pub struct StateStore {
    path: PathBuf,
    pub state: PersistentState,
}

impl StateStore {
    pub fn load(path: &Path) -> Self {
        let state = match fsutil::load_json::<PersistentState>(path) {
            Ok(Some(s)) => s,
            Ok(None) => PersistentState::default(),
            Err(e) => {
                tracing::warn!(error = %e, "実行状態を読み込めないため初期状態で始めます");
                fsutil::quarantine(path);
                PersistentState::default()
            }
        };
        Self {
            path: path.to_path_buf(),
            state,
        }
    }

    pub fn save(&self) -> Result<(), StorageError> {
        fsutil::save_json(&self.path, &self.state)
    }

    /// 期限を過ぎた停止を解除する。解除したら `true`。
    pub fn expire_pause(&mut self, now: DateTime<Utc>) -> bool {
        if self
            .state
            .daily_pause
            .as_ref()
            .is_some_and(|p| !p.is_active(now))
        {
            self.state.daily_pause = None;
            true
        } else {
            false
        }
    }

    pub fn paused(&self, now: DateTime<Utc>) -> bool {
        self.state
            .daily_pause
            .as_ref()
            .is_some_and(|p| p.is_active(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn pause_until_reset_time() {
        let now = Utc::now();
        let p = DailyPause::start(Some(now + TimeDelta::hours(3)), now);
        assert!(p.is_active(now));
        assert!(p.is_active(now + TimeDelta::hours(2)));
        assert!(!p.is_active(now + TimeDelta::hours(3)));
    }

    #[test]
    fn pause_falls_back_to_local_date() {
        let now = Utc::now();
        let p = DailyPause::start(None, now);
        assert!(p.is_active(now));
        assert!(!p.is_active(now + TimeDelta::days(1)));
        // 過去のリセット時刻も日付扱い
        let p = DailyPause::start(Some(now - TimeDelta::hours(1)), now);
        assert!(p.local_date.is_some());
    }

    #[test]
    fn state_persists_and_pause_expires() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        let now = Utc::now();
        let mut s = StateStore::load(&p);
        s.state.daily_pause = Some(DailyPause::start(Some(now + TimeDelta::minutes(1)), now));
        s.state.account = Some(Account {
            user: User {
                id: "u".into(),
                name: "choko1229".into(),
            },
            token_id: "t".into(),
            linked_at: now,
        });
        s.save().unwrap();
        let mut s2 = StateStore::load(&p);
        assert!(s2.paused(now));
        assert!(!s2.expire_pause(now));
        assert!(s2.expire_pause(now + TimeDelta::minutes(2)));
        assert!(!s2.paused(now + TimeDelta::minutes(2)));
        assert_eq!(s2.state.account.unwrap().user.name, "choko1229");
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(
            !raw.to_lowercase().contains("token\":"),
            "トークン本体は保存しない"
        );
    }
}
