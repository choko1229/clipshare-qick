//! 手動確認用のモックサーバー。
//!
//! 使い方: `cargo run -p cq-mock-server -- [ポート]`（既定 8787）。
//! アプリの API ベースURLを `http://127.0.0.1:8787` に設定して使う。
//!
//! 台本の差し込み例（次のアップロードを 503 にする）:
//! `curl -X POST http://127.0.0.1:8787/__mock/script/quick -d '{"type":"error","status":503,"code":"SERVICE_UNAVAILABLE"}'`

use cq_mock_server::{MockConfig, MockServer};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let port = std::env::args()
        .nth(1)
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8787);
    let server = MockServer::start_on(MockConfig::default(), port).await?;
    tracing::info!(url = %server.base_url(), "モックサーバーを起動しました（Ctrl+C で終了）");
    tokio::signal::ctrl_c().await?;
    Ok(())
}
