//! Clipshare API クライアント（契約 v1）。
//!
//! - すべて HTTPS（開発用のループバックのみ http を許可）。証明書検証は無効にしない。
//! - リダイレクトは同じホストへのものだけ追う（更新ファイルのダウンロードは許可リスト）。
//! - ログに `Authorization` ヘッダ・トークン・ファイル内容を出さない。

pub mod error;
pub mod types;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderMap, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use sha2::Digest;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::Url;

pub use error::{ApiError, RetryClass};
pub use types::*;

use crate::media::MediaKind;
use crate::secret::SecretString;
use crate::settings::{Quality, is_loopback_host, validate_api_url};

/// 進捗の通知（送信済みバイト数, 全体バイト数）。
pub type ProgressFn = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// 更新ファイルのダウンロードで追ってよいリダイレクト先。
const DOWNLOAD_REDIRECT_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

/// アップロードする内容。
#[derive(Debug)]
pub enum UploadBody<'a> {
    /// ファイルをそのまま送る（ストリーミング）。
    File(&'a Path),
    /// メモリ上のデータ（縮小した画像など）。
    Memory {
        bytes: Vec<u8>,
        file_name: String,
        mime: &'static str,
    },
}

/// 削除の認証方法。
#[derive(Debug, Clone, Copy)]
pub enum DeleteAuth<'a> {
    Bearer(&'a SecretString),
    DeleteToken(&'a SecretString),
}

/// API クライアント。複製は安価（内部で接続を共有する）。
#[derive(Debug, Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    download_http: reqwest::Client,
    base: Url,
    upload_timeout: Duration,
}

impl ApiClient {
    pub fn new(
        base_url: &str,
        request_timeout: Duration,
        upload_timeout: Duration,
    ) -> Result<Self, ApiError> {
        let mut base = validate_api_url(base_url).map_err(|_| ApiError::InvalidUrl)?;
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let origin_host = base.host_str().map(str::to_owned);
        let allow_http = is_loopback_host(&base);
        let same_host = reqwest::redirect::Policy::custom(move |attempt| {
            let ok_scheme = attempt.url().scheme() == "https"
                || (allow_http && attempt.url().scheme() == "http");
            if attempt.previous().len() >= 5 {
                attempt.error("リダイレクトが多すぎます")
            } else if ok_scheme && attempt.url().host_str() == origin_host.as_deref() {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let http = reqwest::Client::builder()
            .user_agent(crate::user_agent())
            .redirect(same_host)
            .connect_timeout(Duration::from_secs(10))
            .timeout(request_timeout)
            .https_only(!allow_http)
            .build()
            .map_err(ApiError::Transport)?;
        let download_policy = reqwest::redirect::Policy::custom(|attempt| {
            let host = attempt
                .url()
                .host_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if attempt.previous().len() >= 5 {
                attempt.error("リダイレクトが多すぎます")
            } else if attempt.url().scheme() == "https"
                && DOWNLOAD_REDIRECT_HOSTS.contains(&host.as_str())
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let download_http = reqwest::Client::builder()
            .user_agent(crate::user_agent())
            .redirect(download_policy)
            .connect_timeout(Duration::from_secs(10))
            .https_only(!allow_http)
            .build()
            .map_err(ApiError::Transport)?;
        Ok(Self {
            http,
            download_http,
            base,
            upload_timeout,
        })
    }

    pub fn base_url(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str) -> Result<Url, ApiError> {
        self.base.join(path).map_err(|_| ApiError::InvalidUrl)
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&SecretString>,
    ) -> Result<RequestBuilder, ApiError> {
        let mut rb = self.http.request(method, self.url(path)?);
        if let Some(t) = token {
            rb = rb.header(AUTHORIZATION, format!("Bearer {}", t.expose()));
        }
        Ok(rb)
    }

    async fn send(&self, endpoint: &'static str, rb: RequestBuilder) -> Result<Response, ApiError> {
        let resp = rb.send().await.map_err(|e| {
            let err = ApiError::from_reqwest(e);
            tracing::warn!(endpoint, code = err.code(), "API に接続できませんでした");
            err
        })?;
        let status = resp.status();
        tracing::debug!(
            endpoint,
            status = status.as_u16(),
            request_id = request_id(resp.headers()).as_deref().unwrap_or("-"),
            "API 応答"
        );
        if status.is_success() {
            Ok(resp)
        } else {
            Err(error_from_response(endpoint, resp).await)
        }
    }

    /// `GET /api/v1/me`
    pub async fn me(&self, token: Option<&SecretString>) -> Result<MeResponse, ApiError> {
        let rb = self.request(Method::GET, "api/v1/me", token)?;
        let resp = self.send("me", rb).await?;
        resp.json()
            .await
            .map_err(|_| ApiError::UnexpectedResponse { status: 200 })
    }

    /// `POST /api/v1/auth/token`
    pub async fn exchange_token(
        &self,
        code: &str,
        code_verifier: &SecretString,
        redirect_uri: &str,
    ) -> Result<TokenResponse, ApiError> {
        let rb = self
            .request(Method::POST, "api/v1/auth/token", None)?
            .json(&TokenRequest {
                code,
                code_verifier: code_verifier.expose(),
                redirect_uri,
            });
        let resp = self.send("auth/token", rb).await?;
        resp.json()
            .await
            .map_err(|_| ApiError::UnexpectedResponse { status: 200 })
    }

    /// `DELETE /api/v1/auth/token`（自分自身のトークンを失効）。
    pub async fn revoke_token(&self, token: &SecretString) -> Result<(), ApiError> {
        let rb = self.request(Method::DELETE, "api/v1/auth/token", Some(token))?;
        self.send("auth/token:delete", rb).await.map(|_| ())
    }

    /// `POST /api/v1/quick`
    pub async fn upload(
        &self,
        token: Option<&SecretString>,
        body: UploadBody<'_>,
        kind: MediaKind,
        quality: Quality,
        progress: Option<ProgressFn>,
        cancel: &CancellationToken,
    ) -> Result<QuickUploadResponse, ApiError> {
        let part = match body {
            UploadBody::File(path) => {
                let file = tokio::fs::File::open(path).await.map_err(ApiError::Io)?;
                let len = file.metadata().await.map_err(ApiError::Io)?.len();
                let mut sent = 0u64;
                let stream = tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024).map(
                    move |chunk| {
                        if let (Ok(c), Some(p)) = (&chunk, &progress) {
                            sent += c.len() as u64;
                            p(sent, len);
                        }
                        chunk
                    },
                );
                reqwest::multipart::Part::stream_with_length(
                    reqwest::Body::wrap_stream(stream),
                    len,
                )
                .file_name(crate::logging::file_name_of(path))
                .mime_str(crate::media::mime_for(path))
                .map_err(ApiError::Transport)?
            }
            UploadBody::Memory {
                bytes,
                file_name,
                mime,
            } => {
                let len = bytes.len() as u64;
                if let Some(p) = &progress {
                    p(len, len);
                }
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(file_name)
                    .mime_str(mime)
                    .map_err(ApiError::Transport)?
            }
        };
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if kind == MediaKind::Image {
            form = form.text("quality", quality.as_api());
        }
        let rb = self
            .request(Method::POST, "api/v1/quick", token)?
            .timeout(self.upload_timeout)
            .multipart(form);
        let resp = tokio::select! {
            r = self.send("quick", rb) => r?,
            () = cancel.cancelled() => return Err(ApiError::Cancelled),
        };
        if resp.status() != StatusCode::CREATED && resp.status() != StatusCode::OK {
            return Err(ApiError::UnexpectedResponse {
                status: resp.status().as_u16(),
            });
        }
        let parsed: QuickUploadResponse = resp
            .json()
            .await
            .map_err(|_| ApiError::UnexpectedResponse { status: 201 })?;
        self.check_share_url(&parsed.url)?;
        Ok(parsed)
    }

    /// 返ってきた共有URLが API と同じホストの https であることを確認する
    /// （想定外のURLをクリップボードに入れないため）。
    fn check_share_url(&self, url: &str) -> Result<(), ApiError> {
        let u = Url::parse(url).map_err(|_| ApiError::UnexpectedResponse { status: 201 })?;
        let scheme_ok =
            u.scheme() == "https" || (is_loopback_host(&self.base) && u.scheme() == "http");
        if scheme_ok && u.host_str() == self.base.host_str() {
            Ok(())
        } else {
            tracing::warn!("共有URLのホストが想定と異なるため破棄しました");
            Err(ApiError::UnexpectedResponse { status: 201 })
        }
    }

    /// `DELETE /api/v1/quick/{id}`
    pub async fn delete_quick(&self, id: &str, auth: DeleteAuth<'_>) -> Result<(), ApiError> {
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(ApiError::InvalidRequest {
                message: "id".to_owned(),
            });
        }
        let path = format!("api/v1/quick/{id}");
        let rb = match auth {
            DeleteAuth::Bearer(t) => self.request(Method::DELETE, &path, Some(t))?,
            DeleteAuth::DeleteToken(t) => self
                .request(Method::DELETE, &path, None)?
                .header("X-Delete-Token", t.expose()),
        };
        self.send("quick:delete", rb).await.map(|_| ())
    }

    /// `GET /api/v1/client/latest`
    pub async fn latest_client(&self) -> Result<LatestRelease, ApiError> {
        let rb = self.request(Method::GET, "api/v1/client/latest", None)?;
        let resp = self.send("client/latest", rb).await?;
        resp.json()
            .await
            .map_err(|_| ApiError::UnexpectedResponse { status: 200 })
    }

    /// 更新ファイルをダウンロードし、SHA-256（16進小文字）を返す。
    pub async fn download_to(
        &self,
        url: &str,
        dest: &Path,
        max_bytes: u64,
        progress: Option<ProgressFn>,
        cancel: &CancellationToken,
    ) -> Result<String, ApiError> {
        let u = Url::parse(url).map_err(|_| ApiError::InvalidUrl)?;
        let host_ok = u.host_str() == self.base.host_str()
            || u.host_str().is_some_and(|h| {
                DOWNLOAD_REDIRECT_HOSTS.contains(&h.to_ascii_lowercase().as_str())
            });
        let scheme_ok =
            u.scheme() == "https" || (is_loopback_host(&self.base) && u.scheme() == "http");
        if !host_ok || !scheme_ok {
            return Err(ApiError::InvalidUrl);
        }
        let resp = self
            .download_http
            .get(u)
            .send()
            .await
            .map_err(ApiError::from_reqwest)?;
        if !resp.status().is_success() {
            return Err(error_from_response("download", resp).await);
        }
        let total = resp.content_length().unwrap_or(0);
        if total > max_bytes {
            return Err(ApiError::SizeLimitExceeded {
                limit_bytes: Some(max_bytes),
                kind: None,
            });
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ApiError::Io)?;
        }
        let mut file = tokio::fs::File::create(dest).await.map_err(ApiError::Io)?;
        let mut hasher = sha2::Sha256::new();
        let mut received = 0u64;
        let mut stream = resp.bytes_stream();
        loop {
            let next = tokio::select! {
                n = stream.next() => n,
                () = cancel.cancelled() => {
                    drop(file);
                    let _ = tokio::fs::remove_file(dest).await;
                    return Err(ApiError::Cancelled);
                }
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(ApiError::from_reqwest)?;
            received += chunk.len() as u64;
            if received > max_bytes {
                drop(file);
                let _ = tokio::fs::remove_file(dest).await;
                return Err(ApiError::SizeLimitExceeded {
                    limit_bytes: Some(max_bytes),
                    kind: None,
                });
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(ApiError::Io)?;
            if let Some(p) = &progress {
                p(received, total.max(received));
            }
        }
        file.flush().await.map_err(ApiError::Io)?;
        file.sync_all().await.map_err(ApiError::Io)?;
        Ok(hex(&hasher.finalize()))
    }
}

/// バイト列を16進小文字にする。
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn request_id(h: &HeaderMap) -> Option<String> {
    h.get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.len() <= 128)
        .map(str::to_owned)
}

async fn error_from_response(endpoint: &'static str, resp: Response) -> ApiError {
    let status = resp.status();
    let headers = resp.headers().clone();
    let retry_after_header = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = resp.bytes().await.ok();
    let envelope: Option<ErrorEnvelope> =
        body.as_deref().and_then(|b| serde_json::from_slice(b).ok());
    let code = envelope
        .as_ref()
        .map(|e| e.error.code.as_str())
        .unwrap_or("");
    tracing::warn!(
        endpoint,
        status = status.as_u16(),
        code,
        request_id = request_id(&headers).as_deref().unwrap_or("-"),
        "API がエラーを返しました"
    );
    let details = envelope.as_ref().map(|e| &e.error.details);
    let detail_u64 = |k: &str| {
        details
            .and_then(|d| d.get(k))
            .and_then(serde_json::Value::as_u64)
    };
    match (status.as_u16(), code) {
        (_, "UNAUTHORIZED") | (401, _) => ApiError::Unauthorized,
        (_, "FORBIDDEN") | (403, _) => ApiError::Forbidden,
        (_, "NOT_FOUND") | (404, _) => ApiError::NotFound,
        (_, "SIZE_LIMIT_EXCEEDED") | (413, _) => ApiError::SizeLimitExceeded {
            limit_bytes: detail_u64("limitBytes"),
            kind: details
                .and_then(|d| d.get("kind"))
                .and_then(|k| serde_json::from_value(k.clone()).ok()),
        },
        (_, "UNSUPPORTED_MEDIA") | (415, _) => ApiError::UnsupportedMedia,
        (_, "DAILY_LIMIT_EXCEEDED") => ApiError::DailyLimitExceeded {
            resets_at: details
                .and_then(|d| d.get("resetsAt"))
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc)),
        },
        (_, "RATE_LIMITED") | (429, _) => ApiError::RateLimited {
            retry_after: detail_u64("retryAfterSeconds")
                .map(Duration::from_secs)
                .or(retry_after_header),
        },
        (_, "INVALID_REQUEST") | (400, _) => ApiError::InvalidRequest {
            message: envelope.map(|e| e.error.message).unwrap_or_default(),
        },
        (s, _) if s >= 500 => ApiError::Server {
            status: s,
            code: (!code.is_empty()).then(|| code.to_owned()),
        },
        (s, _) => ApiError::UnexpectedResponse { status: s },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_normalized_and_validated() {
        let c = ApiClient::new(
            "https://clipshare.link",
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            c.url("api/v1/me").unwrap().as_str(),
            "https://clipshare.link/api/v1/me"
        );
        let c = ApiClient::new(
            "https://example.com/base",
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            c.url("api/v1/me").unwrap().as_str(),
            "https://example.com/base/api/v1/me"
        );
        assert!(
            ApiClient::new(
                "http://example.com",
                Duration::from_secs(5),
                Duration::from_secs(5)
            )
            .is_err()
        );
    }

    #[test]
    fn share_url_must_match_api_host() {
        let c = ApiClient::new(
            "https://clipshare.link",
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(c.check_share_url("https://clipshare.link/q/abc").is_ok());
        assert!(c.check_share_url("https://evil.example/q/abc").is_err());
        assert!(c.check_share_url("http://clipshare.link/q/abc").is_err());
        assert!(c.check_share_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(hex(&[0x00, 0xab, 0x10]), "00ab10");
    }
}
