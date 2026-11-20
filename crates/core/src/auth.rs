//! ブラウザ認可（ループバック + PKCE）。
//!
//! 1. `code_verifier` / `code_challenge` / `state` / 接続コードを作り、127.0.0.1 のランダムポートで待ち受ける。
//! 2. ブラウザで `/qick/authorize?...` を開く（開くのは UI 側）。
//! 3. `/callback?code=...&state=...` を 1 回だけ受け取り、`state` を検証する。
//! 4. `POST /api/v1/auth/token` でトークンに交換する。
//!
//! 受信はループバックのみ・使い捨て・タイムアウトつき。`state` が一致しない要求は無視して待ち続ける。

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use base64::Engine;
use rand::Rng;
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::api::{ApiClient, ApiError, TokenResponse};
use crate::secret::SecretString;

/// クライアントID（契約で固定）。
pub const CLIENT_ID: &str = "clipshare-quick";
/// 接続コードに使う文字（0/O、1/I/L など紛らわしい文字を除いた英数大文字）。
pub const CONNECTION_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// 認可の失敗。
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("時間切れになりました")]
    Timeout,
    #[error("許可がキャンセルされました")]
    Denied,
    #[error("キャンセルしました")]
    Cancelled,
    #[error("受信の準備ができませんでした")]
    Listen(#[source] std::io::Error),
    #[error("ブラウザからの応答が不正です")]
    InvalidCallback,
    #[error("トークンを取得できませんでした")]
    Exchange(#[source] ApiError),
    #[error("URLを作れませんでした")]
    Url,
}

impl AuthError {
    /// 画面の「エラーコード」欄に出すコード。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Timeout => "AUTH_TIMEOUT",
            Self::Denied => "ACCESS_DENIED",
            Self::Cancelled => "AUTH_CANCELLED",
            Self::Listen(_) => "AUTH_LISTEN_FAILED",
            Self::InvalidCallback => "AUTH_INVALID_CALLBACK",
            Self::Exchange(e) => e.code(),
            Self::Url => "AUTH_INVALID_URL",
        }
    }
}

/// PKCE の組。
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: SecretString,
    pub challenge: String,
}

/// 64 文字の `code_verifier` と S256 の `code_challenge` を作る。
pub fn generate_pkce() -> Pkce {
    let verifier = random_from(UNRESERVED, 64);
    let challenge = challenge_for(&verifier);
    Pkce {
        verifier: SecretString::new(verifier),
        challenge,
    }
}

/// `BASE64URL(SHA256(verifier))`（パディングなし）。
pub fn challenge_for(verifier: &str) -> String {
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// 推測できない `state`。
pub fn random_state() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill(&mut buf[..]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// `XXX-XXX` 形式の接続コード。
pub fn connection_code() -> String {
    let a = random_from(CONNECTION_CODE_ALPHABET, 3);
    let b = random_from(CONNECTION_CODE_ALPHABET, 3);
    format!("{a}-{b}")
}

fn random_from(alphabet: &[u8], len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| {
            let i = rng.random_range(0..alphabet.len());
            char::from(alphabet.get(i).copied().unwrap_or(b'A'))
        })
        .collect()
}

/// 端末名（`device_name`、最大64文字）。
pub fn device_name() -> String {
    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".to_owned());
    name.chars().take(64).collect()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 待ち受け中の認可。
#[derive(Debug)]
pub struct PendingAuth {
    listener: TcpListener,
    pkce: Pkce,
    state: String,
    connection_code: String,
    redirect_uri: String,
    browser_url: Url,
}

/// ブラウザから受け取った認可コード。
#[derive(Debug)]
pub struct AuthCode {
    code: SecretString,
}

impl PendingAuth {
    /// 待ち受けを開始する。`web_base` は Clipshare のURL。
    pub async fn start(web_base: &Url, device_name: &str) -> Result<Self, AuthError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(AuthError::Listen)?;
        let port = listener.local_addr().map_err(AuthError::Listen)?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let pkce = generate_pkce();
        let state = random_state();
        let connection_code = connection_code();
        let mut browser_url = web_base
            .join("qick/authorize")
            .map_err(|_| AuthError::Url)?;
        browser_url
            .query_pairs_mut()
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("connection_code", &connection_code)
            .append_pair("device_name", device_name);
        tracing::info!(port, "ブラウザ認可の受信待ちを開始しました");
        Ok(Self {
            listener,
            pkce,
            state,
            connection_code,
            redirect_uri,
            browser_url,
        })
    }

    pub fn browser_url(&self) -> &Url {
        &self.browser_url
    }
    pub fn connection_code(&self) -> &str {
        &self.connection_code
    }
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// コールバックを待つ。成功したら待ち受けを閉じる（使い捨て）。
    pub async fn wait(
        &self,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<AuthCode, AuthError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let accepted = tokio::select! {
                r = self.listener.accept() => r,
                () = tokio::time::sleep_until(deadline) => return Err(AuthError::Timeout),
                () = cancel.cancelled() => return Err(AuthError::Cancelled),
            };
            let (stream, peer) = match accepted {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "受信に失敗しました");
                    continue;
                }
            };
            if !peer.ip().is_loopback() {
                continue;
            }
            // ブラウザが投機的に開いて何も送らない接続で、本命の接続が待たされないよう短くする。
            match tokio::time::timeout(Duration::from_secs(3), self.handle(stream)).await {
                Ok(Some(result)) => return result,
                Ok(None) => continue,
                Err(_) => continue,
            }
        }
    }

    /// 1 接続を処理する。`None` なら無関係な要求として待ち続ける。
    async fn handle(&self, mut stream: TcpStream) -> Option<Result<AuthCode, AuthError>> {
        let head = read_head(&mut stream).await?;
        let target = parse_request_target(&head)?;
        let url = Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
        if url.path() != "/callback" {
            let _ = respond(&mut stream, "404 Not Found", "").await;
            return None;
        }
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                "error" => error = Some(v.into_owned()),
                _ => {}
            }
        }
        let state_ok = state
            .as_deref()
            .is_some_and(|s| ct_eq(s.as_bytes(), self.state.as_bytes()));
        if !state_ok {
            tracing::warn!("state が一致しないコールバックを無視しました");
            let _ = respond(&mut stream, "400 Bad Request", "").await;
            return None;
        }
        let page = "<!doctype html><meta charset=\"utf-8\"><title>Clipshare Quick</title>\
                    <body style=\"font-family:sans-serif;padding:32px\">Clipshare Quick に戻ってください。</body>";
        let _ = respond(&mut stream, "200 OK", page).await;
        if let Some(e) = error {
            tracing::info!(error = %e, "ブラウザで許可されませんでした");
            return Some(Err(AuthError::Denied));
        }
        match code {
            Some(c) if !c.is_empty() && c.len() <= 512 => Some(Ok(AuthCode {
                code: SecretString::new(c),
            })),
            _ => Some(Err(AuthError::InvalidCallback)),
        }
    }

    /// 認可コードをトークンに交換する。
    pub async fn exchange(
        &self,
        api: &ApiClient,
        code: AuthCode,
    ) -> Result<TokenResponse, AuthError> {
        api.exchange_token(code.code.expose(), &self.pkce.verifier, &self.redirect_uri)
            .await
            .map_err(AuthError::Exchange)
    }
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(chunk.get(..n)?);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_REQUEST_HEAD {
            return None;
        }
    }
    String::from_utf8(buf).ok()
}

fn parse_request_target(head: &str) -> Option<String> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    (method == "GET" && target.starts_with('/')).then(|| target.to_owned())
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_example() {
        // 期待値は別実装（Python の hashlib + base64.urlsafe_b64encode、パディング除去）で計算した。
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K1uhbF1Tw4xT5JvwTYcfH7S2cM"),
            "ozYFW9O0mur6I-eWp7gnwWG2a__8LHOGZeDmPHV6ZOQ"
        );
        let p = generate_pkce();
        assert!((43..=128).contains(&p.verifier.expose().len()));
        assert_eq!(p.challenge, challenge_for(p.verifier.expose()));
    }

    #[test]
    fn connection_code_format() {
        for _ in 0..100 {
            let c = connection_code();
            assert_eq!(c.len(), 7);
            assert_eq!(&c[3..4], "-");
            assert!(
                c.chars()
                    .filter(|&ch| ch != '-')
                    .all(|ch| CONNECTION_CODE_ALPHABET.contains(&(ch as u8)))
            );
            assert!(!c.contains(['0', 'O', '1', 'I', 'L']));
        }
    }

    #[test]
    fn state_is_random_and_long() {
        let a = random_state();
        let b = random_state();
        assert_ne!(a, b);
        assert!(a.len() >= 43);
    }

    #[test]
    fn request_target_parsing() {
        assert_eq!(
            parse_request_target("GET /callback?code=a HTTP/1.1\r\nHost: x\r\n\r\n").as_deref(),
            Some("/callback?code=a")
        );
        assert!(parse_request_target("POST /callback HTTP/1.1").is_none());
        assert!(parse_request_target("GET http://evil/ HTTP/1.1").is_none());
    }

    async fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    fn port_of(p: &PendingAuth) -> u16 {
        Url::parse(p.redirect_uri()).unwrap().port().unwrap()
    }

    fn query(p: &PendingAuth, k: &str) -> String {
        p.browser_url()
            .query_pairs()
            .find(|(kk, _)| kk == k)
            .map(|(_, v)| v.into_owned())
            .unwrap()
    }

    #[tokio::test]
    async fn browser_url_contains_contract_parameters() {
        let base = Url::parse("https://clipshare.link").unwrap();
        let p = PendingAuth::start(&base, "PC").await.unwrap();
        let u = p.browser_url();
        assert_eq!(u.path(), "/qick/authorize");
        assert_eq!(query(&p, "client_id"), CLIENT_ID);
        assert_eq!(query(&p, "code_challenge_method"), "S256");
        assert_eq!(query(&p, "connection_code"), p.connection_code());
        assert!(query(&p, "redirect_uri").starts_with("http://127.0.0.1:"));
        assert_eq!(query(&p, "device_name"), "PC");
    }

    #[tokio::test]
    async fn wrong_state_is_ignored_then_valid_callback_succeeds() {
        let base = Url::parse("https://clipshare.link").unwrap();
        let p = PendingAuth::start(&base, "PC").await.unwrap();
        let port = port_of(&p);
        let state = query(&p, "state");
        let cancel = CancellationToken::new();
        let waiter = async { p.wait(Duration::from_secs(5), &cancel).await };
        let client = async {
            let r = get(port, "/favicon.ico").await;
            assert!(r.starts_with("HTTP/1.1 404"));
            let r = get(port, "/callback?code=abc&state=wrong").await;
            assert!(r.starts_with("HTTP/1.1 400"));
            let r = get(port, &format!("/callback?code=thecode&state={state}")).await;
            assert!(r.starts_with("HTTP/1.1 200"));
        };
        let (res, ()) = tokio::join!(waiter, client);
        assert_eq!(res.unwrap().code.expose(), "thecode");
    }

    #[tokio::test]
    async fn denied_timeout_and_cancel() {
        let base = Url::parse("https://clipshare.link").unwrap();
        let p = PendingAuth::start(&base, "PC").await.unwrap();
        let port = port_of(&p);
        let state = query(&p, "state");
        let cancel = CancellationToken::new();
        let (res, _) = tokio::join!(p.wait(Duration::from_secs(5), &cancel), async {
            get(
                port,
                &format!("/callback?error=access_denied&state={state}"),
            )
            .await
        });
        assert!(matches!(res, Err(AuthError::Denied)));

        let p = PendingAuth::start(&base, "PC").await.unwrap();
        let res = p.wait(Duration::from_millis(100), &cancel).await;
        assert!(matches!(res, Err(AuthError::Timeout)));
        assert_eq!(res.unwrap_err().code(), "AUTH_TIMEOUT");

        let cancel = CancellationToken::new();
        cancel.cancel();
        let res = p.wait(Duration::from_secs(5), &cancel).await;
        assert!(matches!(res, Err(AuthError::Cancelled)));
    }
}
