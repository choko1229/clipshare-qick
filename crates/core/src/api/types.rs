//! API 契約 v1 の型（`docs/quickshare/API_CONTRACT.md`）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::media::MediaKind;
use crate::secret::SecretString;

/// ユーザー情報。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
}

/// 上限値。`0` は無制限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub image_max_bytes: u64,
    pub video_max_bytes: u64,
    pub daily_limit: u64,
    pub daily_used: u64,
    pub resets_at: DateTime<Utc>,
    pub banner: bool,
}

impl Limits {
    /// 種別ごとの上限（`None` は無制限）。
    pub fn max_bytes(&self, kind: MediaKind) -> Option<u64> {
        let v = match kind {
            MediaKind::Image => self.image_max_bytes,
            MediaKind::Video => self.video_max_bytes,
        };
        (v != 0).then_some(v)
    }

    /// 1日の上限（`None` は無制限）。
    pub fn daily(&self) -> Option<u64> {
        (self.daily_limit != 0).then_some(self.daily_limit)
    }
}

/// `GET /api/v1/me`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    pub authenticated: bool,
    pub user: Option<User>,
    pub limits: Limits,
}

/// `POST /api/v1/auth/token`
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub token: SecretString,
    pub token_id: String,
    pub token_type: String,
    pub user: User,
    pub limits: Limits,
}

/// `POST /api/v1/auth/token` のリクエスト。
#[derive(Debug, Serialize)]
pub(crate) struct TokenRequest<'a> {
    pub code: &'a str,
    pub code_verifier: &'a str,
    pub redirect_uri: &'a str,
}

/// 共有の処理状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RemoteStatus {
    Processing,
    Ready,
    Failed,
}

/// `POST /api/v1/quick` の 201 応答。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickUploadResponse {
    pub id: String,
    pub url: String,
    pub kind: MediaKind,
    pub status: RemoteStatus,
    pub expires_at: DateTime<Utc>,
    /// 匿名アップロードのときだけ返る削除用トークン。
    #[serde(default)]
    pub delete_token: Option<SecretString>,
}

/// 更新ファイル。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub size_bytes: u64,
    /// GitHub が digest を返さない場合は null（契約 v1.1）。
    #[serde(default)]
    pub sha256: Option<String>,
}

/// `GET /api/v1/client/latest`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestRelease {
    pub version: String,
    pub published_at: DateTime<Utc>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub min_supported_version: Option<String>,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

/// エラー応答 `{ "error": { code, message, details } }`。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ErrorBody {
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub details: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_contract_examples() {
        let up: QuickUploadResponse = serde_json::from_str(
            r#"{ "id": "abc123XYZ0", "url": "https://clipshare.link/q/abc123XYZ0", "kind": "IMAGE", "status": "READY",
                 "expiresAt": "2026-11-21T00:00:00Z", "deleteToken": "dt" }"#,
        )
        .unwrap();
        assert_eq!(up.kind, MediaKind::Image);
        assert_eq!(up.status, RemoteStatus::Ready);
        assert_eq!(up.delete_token.unwrap().expose(), "dt");

        let me: MeResponse = serde_json::from_str(
            r#"{"authenticated":false,"user":null,"limits":{"imageMaxBytes":20971520,"videoMaxBytes":0,
                "dailyLimit":5,"dailyUsed":4,"resetsAt":"2026-11-21T15:00:00Z","banner":true}}"#,
        )
        .unwrap();
        assert!(!me.authenticated);
        assert_eq!(me.limits.max_bytes(MediaKind::Video), None);
        assert_eq!(me.limits.max_bytes(MediaKind::Image), Some(20_971_520));
        assert_eq!(me.limits.daily(), Some(5));

        let latest: LatestRelease = serde_json::from_str(
            r#"{ "version": "1.2.3", "publishedAt": "2026-11-20T00:00:00Z", "notes": "n", "minSupportedVersion": null,
                 "assets": [ { "name": "ClipshareQuick-1.2.3.msix", "url": "https://github.com/x", "sizeBytes": 10, "sha256": "ab" } ] }"#,
        )
        .unwrap();
        assert_eq!(latest.assets.len(), 1);
    }

    #[test]
    fn token_response_debug_hides_token() {
        let t: TokenResponse = serde_json::from_str(
            r#"{"token":"secret-token","tokenId":"t1","tokenType":"Bearer","user":{"id":"u","name":"n"},
                "limits":{"imageMaxBytes":0,"videoMaxBytes":0,"dailyLimit":0,"dailyUsed":0,"resetsAt":"2026-11-21T15:00:00Z","banner":false}}"#,
        )
        .unwrap();
        assert!(!format!("{t:?}").contains("secret-token"));
    }
}
