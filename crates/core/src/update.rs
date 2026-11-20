//! 更新の確認と検証（ダウンロード・適用の手前まで）。
//!
//! - 確認頻度は「起動時 + 設定の間隔（既定 24 時間）」。
//! - 新しいバージョンのときだけ通知する（同じ・古いバージョンは無視＝ダウングレード防止）。
//! - ダウンロードしたファイルの SHA-256 が `client/latest` の値と一致しなければ適用しない。
//!   署名の検証（Authenticode）は適用側（アプリ）で行う。

use chrono::{DateTime, TimeDelta, Utc};
use semver::Version;

use crate::api::{LatestRelease, ReleaseAsset};

/// 更新の判定結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    UpToDate,
    Available {
        version: Version,
        asset: ReleaseAsset,
        notes: String,
        /// 今のバージョンがサポート外（更新を強く勧める）。
        required: bool,
    },
    /// 新しいバージョンはあるが、配布ファイルの SHA-256 が無いためアプリ内では適用しない
    /// （ダウンロードページへ案内する。契約 v1.1 で sha256 は null になりうる）。
    ManualOnly {
        version: Version,
        notes: String,
    },
    /// 新しいバージョンはあるが配布ファイル（.msix）が無い。
    NoAsset,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("バージョンの形式が不正です")]
    BadVersion,
    #[error("ダウンロードしたファイルが想定と一致しません")]
    HashMismatch,
    #[error("ダウンロードしたファイルのサイズが想定と一致しません")]
    SizeMismatch,
}

/// 前回の確認から間隔が空いたか。
pub fn should_check(last: Option<DateTime<Utc>>, now: DateTime<Utc>, interval_hours: u64) -> bool {
    match last {
        None => true,
        Some(t) => {
            let hours = i64::try_from(interval_hours).unwrap_or(24);
            now - t >= TimeDelta::hours(hours) || t > now
        }
    }
}

/// 配布ファイル名の拡張子（MSIX）。
const PACKAGE_EXT: &str = ".msix";

/// `latest` を見て更新するか決める。
pub fn decide(current: &str, latest: &LatestRelease) -> Result<UpdateDecision, UpdateError> {
    let cur = Version::parse(current).map_err(|_| UpdateError::BadVersion)?;
    let new = Version::parse(latest.version.trim_start_matches('v'))
        .map_err(|_| UpdateError::BadVersion)?;
    if new <= cur || !new.pre.is_empty() {
        return Ok(UpdateDecision::UpToDate);
    }
    let required = latest
        .min_supported_version
        .as_deref()
        .and_then(|m| Version::parse(m.trim_start_matches('v')).ok())
        .is_some_and(|m| cur < m);
    let packages: Vec<&ReleaseAsset> = latest
        .assets
        .iter()
        .filter(|a| a.name.to_ascii_lowercase().ends_with(PACKAGE_EXT) && a.size_bytes > 0)
        .collect();
    let verifiable = packages.iter().find(|a| {
        a.sha256
            .as_deref()
            .is_some_and(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
    });
    match (verifiable, packages.is_empty()) {
        (Some(a), _) => Ok(UpdateDecision::Available {
            version: new,
            asset: (*a).clone(),
            notes: latest.notes.clone(),
            required,
        }),
        (None, false) => Ok(UpdateDecision::ManualOnly {
            version: new,
            notes: latest.notes.clone(),
        }),
        (None, true) => Ok(UpdateDecision::NoAsset),
    }
}

/// ダウンロード結果を検証する。
pub fn verify_download(
    asset: &ReleaseAsset,
    actual_sha256: &str,
    actual_size: u64,
) -> Result<(), UpdateError> {
    if actual_size != asset.size_bytes {
        return Err(UpdateError::SizeMismatch);
    }
    // 期待値が無いものは検証できないので適用しない。
    let expected = asset.sha256.as_deref().ok_or(UpdateError::HashMismatch)?;
    if !actual_sha256.eq_ignore_ascii_case(expected) {
        return Err(UpdateError::HashMismatch);
    }
    Ok(())
}

/// ファイルサイズの表示（例: `12.3 MB`）。
pub fn format_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else if mb >= 0.1 {
        format!("{mb:.1} MB")
    } else {
        format!("{} KB", (bytes / 1024).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(v: &str, min: Option<&str>, assets: Vec<ReleaseAsset>) -> LatestRelease {
        LatestRelease {
            version: v.into(),
            published_at: Utc::now(),
            notes: "n".into(),
            min_supported_version: min.map(Into::into),
            assets,
        }
    }

    fn msix() -> ReleaseAsset {
        ReleaseAsset {
            name: "ClipshareQuick-1.2.3.msix".into(),
            url: "https://github.com/choko1229/clipshare-qick/releases/download/v1.2.3/ClipshareQuick-1.2.3.msix".into(),
            size_bytes: 10,
            sha256: Some("a".repeat(64)),
        }
    }

    #[test]
    fn only_newer_versions_are_offered() {
        assert_eq!(
            decide("1.2.3", &latest("1.2.3", None, vec![msix()])).unwrap(),
            UpdateDecision::UpToDate
        );
        assert_eq!(
            decide("1.2.3", &latest("1.0.0", None, vec![msix()])).unwrap(),
            UpdateDecision::UpToDate
        );
        assert_eq!(
            decide("1.0.0", &latest("2.0.0-beta.1", None, vec![msix()])).unwrap(),
            UpdateDecision::UpToDate
        );
        match decide("1.0.0", &latest("v1.2.3", Some("1.1.0"), vec![msix()])).unwrap() {
            UpdateDecision::Available {
                version, required, ..
            } => {
                assert_eq!(version, Version::new(1, 2, 3));
                assert!(required);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            decide("1.0.0", &latest("1.2.3", None, vec![])).unwrap(),
            UpdateDecision::NoAsset
        );
        let mut bad = msix();
        bad.sha256 = Some("zz".into());
        // 検証できないハッシュはアプリ内では適用せず、ダウンロードページへ案内する
        assert!(matches!(
            decide("1.0.0", &latest("1.2.3", None, vec![bad])).unwrap(),
            UpdateDecision::ManualOnly { .. }
        ));
        assert!(decide("x", &latest("1.2.3", None, vec![])).is_err());
    }

    #[test]
    fn missing_sha256_is_manual_only_and_never_verified() {
        let mut a = msix();
        a.sha256 = None;
        match decide("1.0.0", &latest("1.2.3", None, vec![a.clone()])).unwrap() {
            UpdateDecision::ManualOnly { version, .. } => {
                assert_eq!(version, Version::new(1, 2, 3))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            verify_download(&a, &"a".repeat(64), 10),
            Err(UpdateError::HashMismatch)
        );
        let j: LatestRelease = serde_json::from_str(
            r#"{"version":"1.2.3","publishedAt":"2026-11-21T00:00:00Z","assets":[{"name":"x.msix","url":"u","sizeBytes":1,"sha256":null}]}"#,
        )
        .unwrap();
        assert!(j.assets[0].sha256.is_none());
    }

    #[test]
    fn verifies_hash_and_size() {
        let a = msix();
        assert!(verify_download(&a, &"A".repeat(64), 10).is_ok());
        assert_eq!(
            verify_download(&a, &"b".repeat(64), 10),
            Err(UpdateError::HashMismatch)
        );
        assert_eq!(
            verify_download(&a, &"a".repeat(64), 11),
            Err(UpdateError::SizeMismatch)
        );
    }

    #[test]
    fn check_interval() {
        let now = Utc::now();
        assert!(should_check(None, now, 24));
        assert!(!should_check(Some(now - TimeDelta::hours(1)), now, 24));
        assert!(should_check(Some(now - TimeDelta::hours(25)), now, 24));
        assert!(
            should_check(Some(now + TimeDelta::hours(5)), now, 24),
            "時計が戻った場合も確認する"
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(62 * 1024 * 1024), "62 MB");
        assert_eq!(format_size(1_572_864), "1.5 MB");
        assert_eq!(format_size(2048), "2 KB");
    }
}
