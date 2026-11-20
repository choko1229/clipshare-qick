//! 1 ファイルのアップロード（再試行・サイズ超過時の縮小）。
//!
//! - 送信前に拡張子と中身（マジックバイト）が一致するか確認する。
//! - 上限が分かっていれば先に確認する（画像は縮小、動画はエラー）。
//! - 413 `SIZE_LIMIT_EXCEEDED`: 画像は長辺 2000px に縮小して 1 回だけ送り直す。動画は縮小できないのでエラー。
//! - 5xx・タイムアウト・接続エラー: 指数バックオフで再試行（回数・間隔は設定値）。
//! - 429 `RATE_LIMITED`: 指定秒数待って再試行。429 `DAILY_LIMIT_EXCEEDED`・401 などは再試行しない。

use std::path::Path;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::api::{
    ApiClient, ApiError, Limits, ProgressFn, QuickUploadResponse, RetryClass, UploadBody,
};
use crate::media::{self, MediaKind, SHRINK_LONG_EDGE};
use crate::secret::SecretString;
use crate::settings::{Quality, RetrySettings};

/// アップロードの結果。
#[derive(Debug)]
pub struct Uploaded {
    pub response: QuickUploadResponse,
    /// サイズ超過のため縮小して送ったか。
    pub shrunk: bool,
}

/// アップロードの失敗。
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("動画は縮小できないため送れませんでした")]
    VideoTooLarge { limit_bytes: Option<u64> },
    #[error("縮小してもサイズが上限を超えています")]
    StillTooLarge,
    #[error("ファイルの中身が形式と一致しません")]
    ContentMismatch,
    #[error("画像を縮小できませんでした")]
    Shrink(#[from] media::ShrinkError),
    #[error(transparent)]
    Api(#[from] ApiError),
}

impl UploadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::VideoTooLarge { .. } => "VIDEO_TOO_LARGE",
            Self::StillTooLarge => "SIZE_LIMIT_EXCEEDED",
            Self::ContentMismatch => "UNSUPPORTED_MEDIA",
            Self::Shrink(_) => "SHRINK_FAILED",
            Self::Api(e) => e.code(),
        }
    }

    /// 履歴・トーストに出す日本語。
    pub fn user_message(&self) -> String {
        match self {
            Self::VideoTooLarge { .. } => "動画は縮小できないため送れませんでした".to_owned(),
            Self::StillTooLarge => "縮小してもサイズが上限を超えています".to_owned(),
            Self::ContentMismatch => "対応していないファイル形式です".to_owned(),
            Self::Shrink(_) => "画像を縮小できませんでした".to_owned(),
            Self::Api(e) => match e {
                ApiError::Unauthorized => "連携が切れています。再ログインしてください".to_owned(),
                ApiError::DailyLimitExceeded { .. } => "今日の上限に達しました".to_owned(),
                ApiError::Offline(_) => "接続待ち".to_owned(),
                ApiError::Timeout
                | ApiError::Transport(_)
                | ApiError::Server { .. }
                | ApiError::RateLimited { .. } => "送信に失敗しました".to_owned(),
                ApiError::UnsupportedMedia => "対応していないファイル形式です".to_owned(),
                ApiError::Io(_) => "ファイルを読み込めませんでした".to_owned(),
                other => other.to_string(),
            },
        }
    }

    /// 手動で再試行して意味があるか。
    pub fn retryable(&self) -> bool {
        match self {
            Self::Api(e) => !matches!(
                e,
                ApiError::UnsupportedMedia | ApiError::InvalidRequest { .. } | ApiError::Forbidden
            ),
            Self::VideoTooLarge { .. } | Self::StillTooLarge | Self::ContentMismatch => false,
            Self::Shrink(_) => true,
        }
    }

    /// 接続待ちにするべき失敗か。
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Api(ApiError::Offline(_)))
    }
}

/// 1 件分の入力。
#[derive(Debug)]
pub struct UploadJob<'a> {
    pub path: &'a Path,
    pub kind: MediaKind,
    pub quality: Quality,
    pub token: Option<&'a SecretString>,
    /// 直近に取得した上限（無ければ事前確認しない）。
    pub limits: Option<&'a Limits>,
}

/// 指数バックオフの待ち時間（`attempt` は 1 始まりの失敗回数）。
pub fn backoff(retry: &RetrySettings, attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(20);
    let ms = retry
        .initial_backoff_ms
        .saturating_mul(1u64 << exp)
        .min(retry.max_backoff_ms);
    Duration::from_millis(ms)
}

/// 縮小した画像を作る（重い処理なので別スレッドで）。
async fn shrink(path: &Path) -> Result<UploadBody<'static>, UploadError> {
    let p = path.to_path_buf();
    let shrunk = tokio::task::spawn_blocking(move || media::shrink_image(&p, SHRINK_LONG_EDGE))
        .await
        .map_err(|_| UploadError::Api(ApiError::Cancelled))??;
    tracing::info!(
        width = shrunk.width,
        height = shrunk.height,
        bytes = shrunk.bytes.len(),
        "画像を縮小しました"
    );
    Ok(UploadBody::Memory {
        bytes: shrunk.bytes,
        file_name: shrunk.file_name,
        mime: shrunk.mime,
    })
}

/// 1 ファイルを送る。
pub async fn upload_file(
    api: &ApiClient,
    job: UploadJob<'_>,
    retry: &RetrySettings,
    progress: Option<ProgressFn>,
    cancel: &CancellationToken,
) -> Result<Uploaded, UploadError> {
    let path = job.path;
    let file_name = crate::logging::file_name_of(path);
    let ok =
        media::verify_file_kind(path, job.kind).map_err(|e| UploadError::Api(ApiError::Io(e)))?;
    if !ok {
        tracing::warn!(file = %file_name, "拡張子と中身が一致しないため送信しません");
        return Err(UploadError::ContentMismatch);
    }
    let size = std::fs::metadata(path)
        .map_err(|e| UploadError::Api(ApiError::Io(e)))?
        .len();

    let mut shrunk: Option<UploadBody<'static>> = None;
    if let Some(limit) = job.limits.and_then(|l| l.max_bytes(job.kind)) {
        if size > limit {
            match job.kind {
                MediaKind::Video => {
                    return Err(UploadError::VideoTooLarge {
                        limit_bytes: Some(limit),
                    });
                }
                MediaKind::Image => {
                    tracing::info!(file = %file_name, size, limit, "上限を超えるため送信前に縮小します");
                    shrunk = Some(shrink(path).await?);
                }
            }
        }
    }

    let mut failures = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Err(UploadError::Api(ApiError::Cancelled));
        }
        let was_shrunk = shrunk.is_some();
        // 縮小済みなら「縮小」として送る（サーバー側でも長辺2000pxに揃う）。
        let quality = if was_shrunk {
            Quality::Shrink
        } else {
            job.quality
        };
        let body = match &shrunk {
            Some(UploadBody::Memory {
                bytes,
                file_name,
                mime,
            }) => UploadBody::Memory {
                bytes: bytes.clone(),
                file_name: file_name.clone(),
                mime,
            },
            _ => UploadBody::File(path),
        };
        tracing::info!(file = %file_name, kind = ?job.kind, quality = quality.as_api(), attempt = failures + 1, shrunk = was_shrunk, "アップロードを開始します");
        let result = api
            .upload(job.token, body, job.kind, quality, progress.clone(), cancel)
            .await;
        match result {
            Ok(response) => {
                tracing::info!(file = %file_name, id = %response.id, status = ?response.status, "アップロードしました");
                return Ok(Uploaded {
                    response,
                    shrunk: was_shrunk,
                });
            }
            Err(ApiError::SizeLimitExceeded { limit_bytes, .. }) => match job.kind {
                MediaKind::Video => return Err(UploadError::VideoTooLarge { limit_bytes }),
                MediaKind::Image if !was_shrunk => {
                    tracing::info!(file = %file_name, "サイズ超過のため縮小して送り直します");
                    shrunk = Some(shrink(path).await?);
                }
                MediaKind::Image => return Err(UploadError::StillTooLarge),
            },
            Err(e) => {
                let class = e.retry_class();
                failures += 1;
                let wait = match class {
                    RetryClass::Never => return Err(e.into()),
                    RetryClass::Backoff | RetryClass::WaitForConnection => backoff(retry, failures),
                    RetryClass::After(d) => {
                        d.min(Duration::from_millis(retry.max_backoff_ms.max(1_000) * 10))
                    }
                };
                if failures >= retry.max_attempts {
                    tracing::warn!(file = %file_name, code = e.code(), attempts = failures, "再試行の上限に達しました");
                    return Err(e.into());
                }
                tracing::info!(file = %file_name, code = e.code(), wait_ms = wait.as_millis() as u64, "再試行します");
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = cancel.cancelled() => return Err(UploadError::Api(ApiError::Cancelled)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let r = RetrySettings {
            max_attempts: 5,
            initial_backoff_ms: 1_000,
            max_backoff_ms: 5_000,
        };
        assert_eq!(backoff(&r, 1), Duration::from_millis(1_000));
        assert_eq!(backoff(&r, 2), Duration::from_millis(2_000));
        assert_eq!(backoff(&r, 3), Duration::from_millis(4_000));
        assert_eq!(backoff(&r, 4), Duration::from_millis(5_000));
        assert_eq!(backoff(&r, 99), Duration::from_millis(5_000));
    }

    #[test]
    fn messages_and_retryability() {
        assert_eq!(
            UploadError::VideoTooLarge { limit_bytes: None }.user_message(),
            "動画は縮小できないため送れませんでした"
        );
        assert!(!UploadError::VideoTooLarge { limit_bytes: None }.retryable());
        assert!(
            UploadError::Api(ApiError::Server {
                status: 500,
                code: None
            })
            .retryable()
        );
        assert!(!UploadError::Api(ApiError::UnsupportedMedia).retryable());
        assert!(UploadError::Api(ApiError::Unauthorized).retryable());
    }
}
