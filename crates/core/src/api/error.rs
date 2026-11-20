//! API 呼び出しのエラー型と、再試行の分類。

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::media::MediaKind;

/// API 呼び出しの失敗。
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("連携が無効になっています（再ログインが必要です）")]
    Unauthorized,
    #[error("権限がありません")]
    Forbidden,
    #[error("見つかりませんでした")]
    NotFound,
    #[error("リクエストが不正です: {message}")]
    InvalidRequest { message: String },
    #[error("サイズが上限を超えています")]
    SizeLimitExceeded {
        limit_bytes: Option<u64>,
        kind: Option<MediaKind>,
    },
    #[error("対応していないファイル形式です")]
    UnsupportedMedia,
    #[error("今日の上限に達しました")]
    DailyLimitExceeded { resets_at: Option<DateTime<Utc>> },
    #[error("混み合っています")]
    RateLimited { retry_after: Option<Duration> },
    #[error("サーバーでエラーが発生しました（HTTP {status}）")]
    Server { status: u16, code: Option<String> },
    #[error("サーバーに接続できません")]
    Offline(#[source] reqwest::Error),
    #[error("通信がタイムアウトしました")]
    Timeout,
    #[error("通信に失敗しました")]
    Transport(#[source] reqwest::Error),
    #[error("想定外の応答です（HTTP {status}）")]
    UnexpectedResponse { status: u16 },
    #[error("ファイルを読み込めませんでした")]
    Io(#[source] std::io::Error),
    #[error("URLが不正です")]
    InvalidUrl,
    #[error("キャンセルしました")]
    Cancelled,
}

/// 失敗したときにどう再試行するか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// 再試行しない。
    Never,
    /// 指数バックオフで再試行する（5xx・タイムアウト）。
    Backoff,
    /// 指定時間待ってから再試行する（429 RATE_LIMITED）。
    After(Duration),
    /// 接続が戻るまで待つ（オフライン）。
    WaitForConnection,
}

impl ApiError {
    pub fn retry_class(&self) -> RetryClass {
        match self {
            Self::Server { .. } | Self::Timeout | Self::Transport(_) => RetryClass::Backoff,
            Self::RateLimited { retry_after } => {
                RetryClass::After(retry_after.unwrap_or(Duration::from_secs(30)))
            }
            Self::Offline(_) => RetryClass::WaitForConnection,
            Self::Unauthorized
            | Self::Forbidden
            | Self::NotFound
            | Self::InvalidRequest { .. }
            | Self::SizeLimitExceeded { .. }
            | Self::UnsupportedMedia
            | Self::DailyLimitExceeded { .. }
            | Self::UnexpectedResponse { .. }
            | Self::Io(_)
            | Self::InvalidUrl
            | Self::Cancelled => RetryClass::Never,
        }
    }

    /// ログ・診断用のエラーコード（画面の「エラーコード」欄にも使う）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::NotFound => "NOT_FOUND",
            Self::InvalidRequest { .. } => "INVALID_REQUEST",
            Self::SizeLimitExceeded { .. } => "SIZE_LIMIT_EXCEEDED",
            Self::UnsupportedMedia => "UNSUPPORTED_MEDIA",
            Self::DailyLimitExceeded { .. } => "DAILY_LIMIT_EXCEEDED",
            Self::RateLimited { .. } => "RATE_LIMITED",
            Self::Server { .. } => "SERVER_ERROR",
            Self::Offline(_) => "OFFLINE",
            Self::Timeout => "TIMEOUT",
            Self::Transport(_) => "NETWORK_ERROR",
            Self::UnexpectedResponse { .. } => "UNEXPECTED_RESPONSE",
            Self::Io(_) => "FILE_READ_ERROR",
            Self::InvalidUrl => "INVALID_URL",
            Self::Cancelled => "CANCELLED",
        }
    }

    /// reqwest のエラーを分類する。
    pub(crate) fn from_reqwest(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Timeout
        } else if e.is_connect() {
            Self::Offline(e)
        } else {
            Self::Transport(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classes_follow_contract() {
        assert_eq!(ApiError::Unauthorized.retry_class(), RetryClass::Never);
        assert_eq!(
            ApiError::Server {
                status: 503,
                code: None
            }
            .retry_class(),
            RetryClass::Backoff
        );
        assert_eq!(ApiError::Timeout.retry_class(), RetryClass::Backoff);
        assert_eq!(
            ApiError::RateLimited {
                retry_after: Some(Duration::from_secs(5))
            }
            .retry_class(),
            RetryClass::After(Duration::from_secs(5))
        );
        assert_eq!(
            ApiError::DailyLimitExceeded { resets_at: None }.retry_class(),
            RetryClass::Never
        );
        assert_eq!(ApiError::UnsupportedMedia.retry_class(), RetryClass::Never);
        assert_eq!(
            ApiError::InvalidRequest {
                message: String::new()
            }
            .retry_class(),
            RetryClass::Never
        );
    }
}
