//! Clipshare Quick のコアライブラリ。
//!
//! UI に依存しない処理（設定、ログ、API クライアント、認可、トークン保管、履歴、
//! アップロード、フォルダ監視、IPC、更新）をまとめる。

pub mod api;
pub mod auth;
pub mod error;
pub mod fsutil;
pub mod logging;
pub mod media;
pub mod pathcheck;
pub mod paths;
pub mod secret;
pub mod settings;
pub mod token_store;

/// アプリのバージョン（Cargo.toml と同期）。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// API へ送る User-Agent。
pub fn user_agent() -> String {
    format!("ClipshareQuick/{APP_VERSION} (Windows)")
}
