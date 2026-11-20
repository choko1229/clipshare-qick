//! API 契約 v1（`docs/quickshare/API_CONTRACT.md`）のモックサーバー。
//!
//! Clipshare 側の実装が反映される前でも、クライアントの開発・結合テストができるようにする。
//! 契約にあるすべてのエラーコードを [`Scripted`] で再現できる。
//!
//! テストからはライブラリとして [`MockServer::start`] を使う。手動確認用に `cq-mock-server` バイナリもある
//! （`/__mock/*` の管理用エンドポイントで台本を差し込める）。

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use base64::Engine as _;
use chrono::{DateTime, TimeDelta, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;

/// 認可画面での利用者の操作（自動で決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorizeMode {
    /// 許可してコールバックへリダイレクトする。
    Approve,
    /// キャンセル（`error=access_denied`）。
    Deny,
    /// 何もしない（クライアントのタイムアウト確認用）。
    Ignore,
}

/// モックの設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MockConfig {
    pub anon_daily_limit: u64,
    pub user_daily_limit: u64,
    pub anon_image_max: u64,
    pub user_image_max: u64,
    pub anon_video_max: u64,
    pub user_video_max: u64,
    pub authorize_mode: AuthorizeMode,
    /// 動画を `PROCESSING` で返す。
    pub video_processing: bool,
    /// `GET /api/v1/client/latest` の応答（`None` なら 503）。
    pub latest: Option<Value>,
    /// `GET /download/{name}` の中身。
    #[serde(skip)]
    pub download_body: Vec<u8>,
    pub user_name: String,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            anon_daily_limit: 5,
            user_daily_limit: 0,
            anon_image_max: 20 * 1024 * 1024,
            user_image_max: 50 * 1024 * 1024,
            anon_video_max: 200 * 1024 * 1024,
            user_video_max: 200 * 1024 * 1024,
            authorize_mode: AuthorizeMode::Approve,
            video_processing: true,
            latest: None,
            download_body: Vec::new(),
            user_name: "choko1229".to_owned(),
        }
    }
}

/// 台本どおりに返す応答（1 回ごとに消費する）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Scripted {
    /// 任意のエラー（契約のエラー形式で返す）。
    Error {
        status: u16,
        code: String,
        #[serde(default)]
        details: Value,
    },
    /// 429 RATE_LIMITED（`Retry-After` ヘッダつき）。
    RateLimited { retry_after_seconds: u64 },
    /// 指定ミリ秒待ってから通常どおり処理する（タイムアウト確認用）。
    Delay { ms: u64 },
}

/// 台本を差し込めるエンドポイント。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Endpoint {
    Me,
    Token,
    RevokeToken,
    Quick,
    DeleteQuick,
    Latest,
}

/// 受け取ったリクエストの記録（テストでの確認用）。ファイルの中身は持たない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedRequest {
    pub endpoint: Endpoint,
    pub authorized: bool,
    pub user_agent: String,
    pub quality: Option<String>,
    pub file_name: Option<String>,
    pub file_len: Option<u64>,
    pub status: u16,
}

#[derive(Debug, Clone)]
struct CodeInfo {
    challenge: String,
    redirect_uri: String,
    created: DateTime<Utc>,
    used: bool,
}

#[derive(Debug, Clone)]
struct Upload {
    owner_token: Option<String>,
    delete_token: Option<String>,
}

#[derive(Debug, Default)]
struct Inner {
    cfg: MockConfig,
    tokens: HashMap<String, String>,
    codes: HashMap<String, CodeInfo>,
    uploads: HashMap<String, Upload>,
    daily: HashMap<String, u64>,
    script: HashMap<Endpoint, VecDeque<Scripted>>,
    requests: Vec<RecordedRequest>,
    base_url: String,
}

type Shared = Arc<Mutex<Inner>>;

fn lock(s: &Shared) -> MutexGuard<'_, Inner> {
    match s.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// 動作中のモックサーバー。ドロップすると停止する。
#[derive(Debug)]
pub struct MockServer {
    addr: SocketAddr,
    state: Shared,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockServer {
    /// 127.0.0.1 のランダムポート（`port = 0`）または指定ポートで起動する。
    pub async fn start_on(cfg: MockConfig, port: u16) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        let state: Shared = Arc::new(Mutex::new(Inner {
            cfg,
            base_url: format!("http://{addr}"),
            ..Default::default()
        }));
        let app = router(state.clone());
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!(error = %e, "モックサーバーが停止しました");
            }
        });
        Ok(Self { addr, state, task })
    }

    pub async fn start(cfg: MockConfig) -> std::io::Result<Self> {
        Self::start_on(cfg, 0).await
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 次の呼び出しで返す応答を積む。
    pub fn push(&self, endpoint: Endpoint, s: Scripted) {
        lock(&self.state)
            .script
            .entry(endpoint)
            .or_default()
            .push_back(s);
    }

    /// 契約形式のエラーを積む。
    pub fn push_error(&self, endpoint: Endpoint, status: u16, code: &str, details: Value) {
        self.push(
            endpoint,
            Scripted::Error {
                status,
                code: code.to_owned(),
                details,
            },
        );
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state).requests.clone()
    }

    pub fn requests_to(&self, endpoint: Endpoint) -> Vec<RecordedRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.endpoint == endpoint)
            .collect()
    }

    /// 有効なトークンを直接発行する（ブラウザ認可を省くテスト用）。
    pub fn issue_token(&self) -> String {
        let t = format!("mock_{}", random_string(32));
        let mut g = lock(&self.state);
        let name = g.cfg.user_name.clone();
        g.tokens.insert(t.clone(), name);
        t
    }

    /// すべてのトークンを失効させる（401 の確認用）。
    pub fn revoke_all(&self) {
        lock(&self.state).tokens.clear();
    }

    pub fn token_count(&self) -> usize {
        lock(&self.state).tokens.len()
    }

    pub fn upload_count(&self) -> usize {
        lock(&self.state).uploads.len()
    }

    pub fn configure(&self, f: impl FnOnce(&mut MockConfig)) {
        f(&mut lock(&self.state).cfg);
    }

    /// 1日の利用数をリセットする（翌日の再現）。
    pub fn reset_daily(&self) {
        lock(&self.state).daily.clear();
    }
}

/// 契約形式のエラー応答。
fn error(status: StatusCode, code: &str, message: &str, details: Value) -> Response {
    (
        status,
        axum::Json(json!({ "error": { "code": code, "message": message, "details": details } })),
    )
        .into_response()
}

fn random_string(n: usize) -> String {
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut rng = rand::rng();
    (0..n)
        .map(|_| char::from(A.get(rng.random_range(0..A.len())).copied().unwrap_or(b'a')))
        .collect()
}

fn bearer(h: &HeaderMap) -> Option<String> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_owned)
}

fn user_agent(h: &HeaderMap) -> String {
    h.get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// 台本があれば応答を返す。`Delay` は待ってから `None`。
async fn scripted(state: &Shared, ep: Endpoint) -> Option<Response> {
    let next = lock(state)
        .script
        .get_mut(&ep)
        .and_then(VecDeque::pop_front);
    match next? {
        Scripted::Error {
            status,
            code,
            details,
        } => Some(error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            &code,
            "（モック）台本どおりのエラーです",
            details,
        )),
        Scripted::RateLimited {
            retry_after_seconds,
        } => {
            let mut r = error(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "混み合っています",
                json!({ "retryAfterSeconds": retry_after_seconds }),
            );
            if let Ok(v) = retry_after_seconds.to_string().parse() {
                r.headers_mut().insert(header::RETRY_AFTER, v);
            }
            Some(r)
        }
        Scripted::Delay { ms } => {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            None
        }
    }
}

fn record(state: &Shared, r: RecordedRequest) {
    lock(state).requests.push(r);
}

/// 次の日本時間 0 時（UTC で表す）。
fn next_reset(now: DateTime<Utc>) -> DateTime<Utc> {
    let jst = now + TimeDelta::hours(9);
    let next_midnight = jst
        .date_naive()
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0));
    match next_midnight {
        Some(n) => DateTime::<Utc>::from_naive_utc_and_offset(n, Utc) - TimeDelta::hours(9),
        None => now + TimeDelta::hours(24),
    }
}

fn limits_json(g: &Inner, token: Option<&str>) -> Value {
    let authed = token.is_some();
    let identity = token.map_or_else(|| "anon".to_owned(), str::to_owned);
    let used = g.daily.get(&identity).copied().unwrap_or(0);
    let cfg = &g.cfg;
    json!({
        "imageMaxBytes": if authed { cfg.user_image_max } else { cfg.anon_image_max },
        "videoMaxBytes": if authed { cfg.user_video_max } else { cfg.anon_video_max },
        "dailyLimit": if authed { cfg.user_daily_limit } else { cfg.anon_daily_limit },
        "dailyUsed": used,
        "resetsAt": next_reset(Utc::now()).to_rfc3339(),
        "banner": !authed,
    })
}

/// トークンを確認する。ヘッダなし=匿名 `Ok(None)`、不正=401。
fn check_token(g: &Inner, h: &HeaderMap) -> Result<Option<String>, Unauthorized> {
    match bearer(h) {
        None => Ok(None),
        Some(t) if g.tokens.contains_key(&t) => Ok(Some(t)),
        Some(_) => Err(Unauthorized),
    }
}

/// 401 を返すべきことを表す。
#[derive(Debug)]
struct Unauthorized;

impl IntoResponse for Unauthorized {
    fn into_response(self) -> Response {
        error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "トークンが無効です",
            json!({}),
        )
    }
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/qick/authorize", get(authorize))
        .route("/api/v1/auth/token", post(token).delete(revoke_token))
        .route("/api/v1/me", get(me))
        .route("/api/v1/quick", post(quick))
        .route("/api/v1/quick/{id}", delete(delete_quick))
        .route("/api/v1/client/latest", get(latest))
        .route("/download/{name}", get(download))
        .route("/q/{id}", get(share_page))
        .route("/__mock/script/{endpoint}", post(admin_script))
        .route("/__mock/requests", get(admin_requests))
        .route("/__mock/config", post(admin_config))
        .route("/__mock/revoke", post(admin_revoke))
        .layer(DefaultBodyLimit::max(1024 * 1024 * 1024))
        .with_state(state)
}

async fn authorize(
    State(state): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let get = |k: &str| q.get(k).map(String::as_str).unwrap_or_default();
    let redirect = get("redirect_uri");
    let redirect_ok = (redirect.starts_with("http://127.0.0.1:")
        || redirect.starts_with("http://[::1]:"))
        && url_like_port(redirect);
    let code_ok = {
        let c = get("connection_code");
        c.len() == 7 && c.as_bytes().get(3) == Some(&b'-')
    };
    if get("client_id") != "clipshare-quick"
        || !redirect_ok
        || get("code_challenge_method") != "S256"
        || get("code_challenge").len() < 43
        || get("state").is_empty()
        || !code_ok
        || get("device_name").chars().count() > 64
    {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "認可リクエストが不正です",
            json!({}),
        );
    }
    let mode = lock(&state).cfg.authorize_mode;
    let st = urlencode(get("state"));
    match mode {
        AuthorizeMode::Ignore => (StatusCode::OK, "（モック）認可待ち").into_response(),
        AuthorizeMode::Deny => redirect_to(&format!("{redirect}?error=access_denied&state={st}")),
        AuthorizeMode::Approve => {
            let code = random_string(40);
            lock(&state).codes.insert(
                code.clone(),
                CodeInfo {
                    challenge: get("code_challenge").to_owned(),
                    redirect_uri: redirect.to_owned(),
                    created: Utc::now(),
                    used: false,
                },
            );
            redirect_to(&format!("{redirect}?code={code}&state={st}"))
        }
    }
}

fn url_like_port(redirect: &str) -> bool {
    let rest = redirect
        .trim_start_matches("http://127.0.0.1:")
        .trim_start_matches("http://[::1]:");
    let port: String = rest.chars().take_while(char::is_ascii_digit).collect();
    !port.is_empty() && rest[port.len()..].starts_with('/')
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn redirect_to(location: &str) -> Response {
    let mut r = StatusCode::FOUND.into_response();
    if let Ok(v) = location.parse() {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

#[derive(Debug, Deserialize)]
struct TokenReq {
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

async fn token(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let ua = user_agent(&headers);
    if let Some(r) = scripted(&state, Endpoint::Token).await {
        record_status(&state, Endpoint::Token, false, ua, &r);
        return r;
    }
    let Ok(req) = serde_json::from_slice::<TokenReq>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "形式が不正です",
            json!({}),
        );
    };
    let mut g = lock(&state);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(req.code_verifier.as_bytes()));
    let valid = match g.codes.get_mut(&req.code) {
        Some(c)
            if !c.used
                && c.redirect_uri == req.redirect_uri
                && c.challenge == challenge
                && Utc::now() - c.created < TimeDelta::minutes(10)
                && (43..=128).contains(&req.code_verifier.len()) =>
        {
            c.used = true;
            true
        }
        Some(c) => {
            c.used = true;
            false
        }
        None => false,
    };
    if !valid {
        drop(g);
        let r = error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "認可コードが無効です",
            json!({}),
        );
        record_status(&state, Endpoint::Token, false, ua, &r);
        return r;
    }
    let t = format!("mock_{}", random_string(32));
    let name = g.cfg.user_name.clone();
    g.tokens.insert(t.clone(), name.clone());
    let limits = limits_json(&g, Some(&t));
    drop(g);
    record(
        &state,
        RecordedRequest {
            endpoint: Endpoint::Token,
            authorized: false,
            user_agent: ua,
            quality: None,
            file_name: None,
            file_len: None,
            status: 200,
        },
    );
    axum::Json(json!({
        "token": t,
        "tokenId": format!("tok_{}", random_string(8)),
        "tokenType": "Bearer",
        "user": { "id": "u_mock", "name": name },
        "limits": limits,
    }))
    .into_response()
}

fn record_status(
    state: &Shared,
    endpoint: Endpoint,
    authorized: bool,
    user_agent: String,
    r: &Response,
) {
    record(
        state,
        RecordedRequest {
            endpoint,
            authorized,
            user_agent,
            quality: None,
            file_name: None,
            file_len: None,
            status: r.status().as_u16(),
        },
    );
}

async fn revoke_token(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let ua = user_agent(&headers);
    if let Some(r) = scripted(&state, Endpoint::RevokeToken).await {
        record_status(&state, Endpoint::RevokeToken, true, ua, &r);
        return r;
    }
    let mut g = lock(&state);
    let r = match bearer(&headers) {
        Some(t) if g.tokens.remove(&t).is_some() => StatusCode::NO_CONTENT.into_response(),
        _ => error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "トークンが無効です",
            json!({}),
        ),
    };
    drop(g);
    record_status(&state, Endpoint::RevokeToken, true, ua, &r);
    r
}

async fn me(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let ua = user_agent(&headers);
    if let Some(r) = scripted(&state, Endpoint::Me).await {
        record_status(&state, Endpoint::Me, bearer(&headers).is_some(), ua, &r);
        return r;
    }
    let g = lock(&state);
    let r = match check_token(&g, &headers) {
        Err(r) => r.into_response(),
        Ok(t) => {
            let user = t
                .as_ref()
                .and_then(|t| g.tokens.get(t))
                .map(|n| json!({ "id": "u_mock", "name": n }));
            axum::Json(json!({
                "authenticated": t.is_some(),
                "user": user,
                "limits": limits_json(&g, t.as_deref()),
            }))
            .into_response()
        }
    };
    drop(g);
    record_status(&state, Endpoint::Me, bearer(&headers).is_some(), ua, &r);
    r
}

fn sniff(head: &[u8]) -> Option<&'static str> {
    let starts = |sig: &[u8]| head.len() >= sig.len() && &head[..sig.len()] == sig;
    if starts(b"\x89PNG\r\n\x1a\n") || starts(b"\xFF\xD8\xFF") {
        return Some("IMAGE");
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some("IMAGE");
    }
    if head.len() >= 8 && matches!(&head[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free") {
        return Some("VIDEO");
    }
    if starts(b"\x1A\x45\xDF\xA3") {
        return Some("VIDEO");
    }
    None
}

async fn quick(State(state): State<Shared>, headers: HeaderMap, mut mp: Multipart) -> Response {
    let ua = user_agent(&headers);
    let authorized = bearer(&headers).is_some();
    // 本文を先に読み切る（台本でエラーを返す場合も、送信自体は完了させる）。
    let mut file: Option<(String, Bytes)> = None;
    let mut quality: Option<String> = None;
    loop {
        match mp.next_field().await {
            Ok(Some(field)) => {
                let name = field.name().unwrap_or_default().to_owned();
                if name == "file" {
                    let fname = field.file_name().unwrap_or("upload").to_owned();
                    match field.bytes().await {
                        Ok(b) => file = Some((fname, b)),
                        Err(_) => {
                            return error(
                                StatusCode::BAD_REQUEST,
                                "INVALID_REQUEST",
                                "本文を読めません",
                                json!({}),
                            );
                        }
                    }
                } else if name == "quality" {
                    quality = field.text().await.ok();
                }
            }
            Ok(None) => break,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "INVALID_REQUEST",
                    "形式が不正です",
                    json!({}),
                );
            }
        }
    }
    let mut rec = RecordedRequest {
        endpoint: Endpoint::Quick,
        authorized,
        user_agent: ua,
        quality: quality.clone(),
        file_name: file.as_ref().map(|f| f.0.clone()),
        file_len: file.as_ref().map(|f| f.1.len() as u64),
        status: 0,
    };
    // 遅延の台本は、待つ前に到着を記録する（クライアントがタイムアウトで切断すると処理が中断されるため）。
    let delayed = matches!(
        lock(&state)
            .script
            .get(&Endpoint::Quick)
            .and_then(VecDeque::front),
        Some(Scripted::Delay { .. })
    );
    if delayed {
        record(
            &state,
            RecordedRequest {
                status: 0,
                ..rec.clone()
            },
        );
    }
    if let Some(r) = scripted(&state, Endpoint::Quick).await {
        rec.status = r.status().as_u16();
        record(&state, rec);
        return r;
    }
    let r = quick_inner(&state, &headers, file, quality.as_deref());
    rec.status = r.status().as_u16();
    record(&state, rec);
    r
}

fn quick_inner(
    state: &Shared,
    headers: &HeaderMap,
    file: Option<(String, Bytes)>,
    quality: Option<&str>,
) -> Response {
    let mut g = lock(state);
    let token = match check_token(&g, headers) {
        Ok(t) => t,
        Err(r) => return r.into_response(),
    };
    let Some((_, bytes)) = file else {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "file がありません",
            json!({}),
        );
    };
    if let Some(q) = quality {
        if q != "original" && q != "shrink" {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                "quality が不正です",
                json!({}),
            );
        }
    }
    let Some(kind) = sniff(&bytes) else {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA",
            "対応していない形式です",
            json!({}),
        );
    };
    let authed = token.is_some();
    let cfg = g.cfg.clone();
    let max = match (kind, authed) {
        ("IMAGE", true) => cfg.user_image_max,
        ("IMAGE", false) => cfg.anon_image_max,
        (_, true) => cfg.user_video_max,
        (_, false) => cfg.anon_video_max,
    };
    if max != 0 && bytes.len() as u64 > max {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "SIZE_LIMIT_EXCEEDED",
            "サイズが上限を超えています",
            json!({ "limitBytes": max, "kind": kind }),
        );
    }
    let identity = token.clone().unwrap_or_else(|| "anon".to_owned());
    let limit = if authed {
        cfg.user_daily_limit
    } else {
        cfg.anon_daily_limit
    };
    let used = g.daily.get(&identity).copied().unwrap_or(0);
    if limit != 0 && used >= limit {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "DAILY_LIMIT_EXCEEDED",
            "今日の上限に達しました",
            json!({ "resetsAt": next_reset(Utc::now()).to_rfc3339() }),
        );
    }
    g.daily.insert(identity, used + 1);
    let id = random_string(10);
    let delete_token = (!authed).then(|| random_string(24));
    g.uploads.insert(
        id.clone(),
        Upload {
            owner_token: token,
            delete_token: delete_token.clone(),
        },
    );
    let status = if kind == "VIDEO" && cfg.video_processing {
        "PROCESSING"
    } else {
        "READY"
    };
    let mut body = json!({
        "id": id,
        "url": format!("{}/q/{id}", g.base_url),
        "kind": kind,
        "status": status,
        "expiresAt": (Utc::now() + TimeDelta::hours(24)).to_rfc3339(),
    });
    if let (Some(dt), Some(obj)) = (delete_token, body.as_object_mut()) {
        obj.insert("deleteToken".to_owned(), Value::String(dt));
    }
    (StatusCode::CREATED, axum::Json(body)).into_response()
}

async fn delete_quick(
    State(state): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let ua = user_agent(&headers);
    if let Some(r) = scripted(&state, Endpoint::DeleteQuick).await {
        record_status(
            &state,
            Endpoint::DeleteQuick,
            bearer(&headers).is_some(),
            ua,
            &r,
        );
        return r;
    }
    let mut g = lock(&state);
    let token = match check_token(&g, &headers) {
        Ok(t) => t,
        Err(r) => return r.into_response(),
    };
    let dt = headers
        .get("x-delete-token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let allowed = g.uploads.get(&id).is_some_and(|u| match (&token, &dt) {
        (Some(t), _) => u.owner_token.as_deref() == Some(t.as_str()),
        (None, Some(d)) => u.delete_token.as_deref() == Some(d.as_str()),
        (None, None) => false,
    });
    let r = if allowed {
        g.uploads.remove(&id);
        StatusCode::NO_CONTENT.into_response()
    } else {
        error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "見つかりません",
            json!({}),
        )
    };
    drop(g);
    record_status(&state, Endpoint::DeleteQuick, token.is_some(), ua, &r);
    r
}

async fn latest(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let ua = user_agent(&headers);
    if let Some(r) = scripted(&state, Endpoint::Latest).await {
        record_status(&state, Endpoint::Latest, false, ua, &r);
        return r;
    }
    let l = lock(&state).cfg.latest.clone();
    let r = match l {
        Some(v) => axum::Json(v).into_response(),
        None => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "取得できません",
            json!({}),
        ),
    };
    record_status(&state, Endpoint::Latest, false, ua, &r);
    r
}

async fn download(State(state): State<Shared>) -> Response {
    let body = lock(&state).cfg.download_body.clone();
    (StatusCode::OK, body).into_response()
}

async fn share_page(Path(id): Path<String>) -> Response {
    let safe: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        format!("<!doctype html><meta charset=utf-8><title>mock {safe}</title><p>（モック）共有ページ {safe}</p>"),
    )
        .into_response()
}

async fn admin_script(
    State(state): State<Shared>,
    Path(ep): Path<String>,
    body: Bytes,
) -> Response {
    let ep: Result<Endpoint, _> = serde_json::from_value(Value::String(ep));
    let s: Result<Scripted, _> = serde_json::from_slice(&body);
    match (ep, s) {
        (Ok(ep), Ok(s)) => {
            lock(&state).script.entry(ep).or_default().push_back(s);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn admin_requests(State(state): State<Shared>) -> Response {
    axum::Json(lock(&state).requests.clone()).into_response()
}

async fn admin_config(State(state): State<Shared>, body: Bytes) -> Response {
    match serde_json::from_slice::<MockConfig>(&body) {
        Ok(mut cfg) => {
            let mut g = lock(&state);
            cfg.download_body = std::mem::take(&mut g.cfg.download_body);
            g.cfg = cfg;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn admin_revoke(State(state): State<Shared>) -> Response {
    lock(&state).tokens.clear();
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_is_next_jst_midnight() {
        let now = DateTime::parse_from_rfc3339("2026-11-20T16:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // JST 2026-11-21 01:30 → 次の 0 時は 2026-11-22 00:00 JST = 2026-11-21T15:00Z
        assert_eq!(next_reset(now).to_rfc3339(), "2026-11-21T15:00:00+00:00");
    }

    #[test]
    fn redirect_port_check() {
        assert!(url_like_port("http://127.0.0.1:5000/callback"));
        assert!(url_like_port("http://[::1]:5000/callback"));
        assert!(!url_like_port("http://127.0.0.1:/callback"));
        assert!(!url_like_port("http://127.0.0.1:80evil.com/"));
    }
}
