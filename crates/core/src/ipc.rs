//! IPC サーバー（本体アプリ側）。プロトコルとクライアントは `cq-ipc` クレート。
//!
//! 最初のインスタンスとしてパイプを作れなければ、すでに本体が動いている（二重起動）とみなす。

pub use cq_ipc::{Request, Response, ShareQuality, View};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum IpcServerError {
    #[error("すでに起動しています")]
    AlreadyRunning,
    #[error("IPC を開始できませんでした")]
    Ipc(#[from] cq_ipc::IpcError),
    #[error("IPC を開始できませんでした")]
    Io(#[source] std::io::Error),
}

/// 動作中のサーバー。ドロップで停止する。
#[derive(Debug)]
pub struct IpcServer {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

#[cfg(windows)]
impl IpcServer {
    /// 受け付けを始める。要求は `tx` に流す。
    pub fn start(tx: mpsc::UnboundedSender<Request>) -> Result<Self, IpcServerError> {
        let sid = cq_ipc::current_user_sid()?;
        let name = cq_ipc::pipe_name(&sid);
        let security = cq_ipc::PipeSecurity::for_user(&sid)?;
        let first = create(&name, &security, true).map_err(|e| {
            if e.raw_os_error() == Some(5) || e.raw_os_error() == Some(231) {
                IpcServerError::AlreadyRunning
            } else {
                IpcServerError::Io(e)
            }
        })?;
        let cancel = CancellationToken::new();
        let task = tokio::spawn(accept_loop(name, security, first, tx, cancel.clone()));
        tracing::info!("IPC の受け付けを開始しました");
        Ok(Self { cancel, task })
    }
}

#[cfg(windows)]
fn create(
    name: &str,
    security: &cq_ipc::PipeSecurity,
    first: bool,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    opts.first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(8)
        .in_buffer_size(64 * 1024)
        .out_buffer_size(4 * 1024);
    // SAFETY: `security` は呼び出しの間生存している SECURITY_ATTRIBUTES を指す。
    unsafe { opts.create_with_security_attributes_raw(name, security.as_ptr()) }
}

#[cfg(windows)]
async fn accept_loop(
    name: String,
    security: cq_ipc::PipeSecurity,
    mut server: tokio::net::windows::named_pipe::NamedPipeServer,
    tx: mpsc::UnboundedSender<Request>,
    cancel: CancellationToken,
) {
    loop {
        let connected = tokio::select! {
            r = server.connect() => r,
            () = cancel.cancelled() => return,
        };
        if let Err(e) = &connected {
            tracing::warn!(error = %e, "IPC の接続受け付けに失敗しました");
        }
        let next = match create(&name, &security, false) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "IPC の次の受け付けを用意できません");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };
        let conn = std::mem::replace(&mut server, next);
        if connected.is_ok() {
            let tx = tx.clone();
            tokio::spawn(async move {
                let r = tokio::time::timeout(std::time::Duration::from_secs(5), handle(conn, &tx))
                    .await;
                if r.is_err() {
                    tracing::warn!("IPC の要求がタイムアウトしました");
                }
            });
        }
    }
}

#[cfg(windows)]
async fn handle(
    mut conn: tokio::net::windows::named_pipe::NamedPipeServer,
    tx: &mpsc::UnboundedSender<Request>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let line = loop {
        let n = match conn.read(&mut chunk).await {
            Ok(n) => n,
            Err(_) => return,
        };
        if n == 0 {
            return;
        }
        let Some(c) = chunk.get(..n) else { return };
        if let Some(pos) = c.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(c.get(..pos).unwrap_or_default());
            break buf;
        }
        buf.extend_from_slice(c);
        if buf.len() > cq_ipc::MAX_MESSAGE_BYTES {
            return;
        }
    };
    let response = match cq_ipc::decode::<Request>(&line) {
        Ok(Request::Share { paths, .. })
            if paths.is_empty() || paths.len() > cq_ipc::MAX_SHARE_FILES =>
        {
            Response::Rejected {
                reason: "ファイルの数が多すぎます".to_owned(),
            }
        }
        Ok(req) => {
            if tx.send(req).is_ok() {
                Response::Accepted
            } else {
                Response::Rejected {
                    reason: "終了処理中です".to_owned(),
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "IPC の要求を読めませんでした");
            Response::Rejected {
                reason: "要求の形式が不正です".to_owned(),
            }
        }
    };
    if let Ok(bytes) = cq_ipc::encode(&response) {
        let _ = conn.write_all(&bytes).await;
        let _ = conn.flush().await;
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    /// 実際の名前付きパイプで往復する（同じ利用者のパイプ名を使うため、本体が動いていない環境で実行する）。
    #[tokio::test(flavor = "multi_thread")]
    async fn server_accepts_share_and_rejects_second_instance() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let server = match IpcServer::start(tx) {
            Ok(s) => s,
            Err(IpcServerError::AlreadyRunning) => return, // 開発機で本体が動作中
            Err(e) => panic!("{e}"),
        };
        let (tx2, _rx2) = mpsc::unbounded_channel();
        assert!(matches!(
            IpcServer::start(tx2),
            Err(IpcServerError::AlreadyRunning)
        ));

        let req = Request::Share {
            quality: ShareQuality::Original,
            paths: vec![PathBuf::from(r"C:\x.png")],
        };
        let req2 = req.clone();
        let resp = tokio::task::spawn_blocking(move || {
            cq_ipc::send_request(&req2, Duration::from_secs(2))
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resp, Response::Accepted);
        assert_eq!(rx.recv().await.unwrap(), req);

        let empty = Request::Share {
            quality: ShareQuality::Default,
            paths: vec![],
        };
        let resp = tokio::task::spawn_blocking(move || {
            cq_ipc::send_request(&empty, Duration::from_secs(2))
        })
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(resp, Response::Rejected { .. }));
        drop(server);
    }
}
