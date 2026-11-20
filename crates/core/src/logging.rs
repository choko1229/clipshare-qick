//! ログ基盤（ローテーション・レベル切替・機微情報のマスク）。
//!
//! 方針:
//! - ログにトークン、`Authorization` ヘッダ、ファイルの内容を出さない。
//!   HTTP は URL のパスとステータス、API エラーコードのみ記録する。
//! - ユーザー名を含むパスは `%USERPROFILE%` に置き換えて記録する（[`mask_path`]）。
//! - 日単位でローテーションし、既定で 7 日分を残す。

use std::path::Path;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

use crate::settings::LogLevel;

/// ログを残す日数。
pub const LOG_RETENTION_FILES: usize = 7;

/// ログの初期化結果。ドロップするとバッファが書き出されなくなるので、アプリ終了まで保持する。
pub struct LogGuard {
    _worker: tracing_appender::non_blocking::WorkerGuard,
    handle: reload::Handle<EnvFilter, Registry>,
}

impl std::fmt::Debug for LogGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogGuard").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogInitError {
    #[error("ログフォルダを作成できませんでした")]
    Dir(#[source] std::io::Error),
    #[error("ログファイルを開けませんでした")]
    Appender(#[source] tracing_appender::rolling::InitError),
    #[error("ログを初期化できませんでした")]
    Init(#[source] tracing_subscriber::util::TryInitError),
}

fn filter_for(level: LogLevel) -> EnvFilter {
    // 依存クレートの冗長なログは warn 以上に抑える。
    EnvFilter::new(format!(
        "warn,cq_core={0},clipshare_quick={0},cq_vr={0},cq_shell_ext={0}",
        level.as_filter()
    ))
}

impl LogGuard {
    /// ログレベルを即時に切り替える。
    pub fn set_level(&self, level: LogLevel) {
        if let Err(e) = self.handle.modify(|f| *f = filter_for(level)) {
            tracing::warn!(error = %e, "ログレベルを変更できませんでした");
        } else {
            tracing::info!(level = level.as_filter(), "ログレベルを変更しました");
        }
    }
}

/// ファイルへのログ出力を開始する。
pub fn init(dir: &Path, level: LogLevel) -> Result<LogGuard, LogInitError> {
    std::fs::create_dir_all(dir).map_err(LogInitError::Dir)?;
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("clipshare-quick")
        .filename_suffix("log")
        .max_log_files(LOG_RETENTION_FILES)
        .build(dir)
        .map_err(LogInitError::Appender)?;
    let (writer, worker) = tracing_appender::non_blocking(appender);
    let (filter, handle) = reload::Layer::new(filter_for(level));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(true),
        )
        .try_init()
        .map_err(LogInitError::Init)?;
    Ok(LogGuard {
        _worker: worker,
        handle,
    })
}

/// パスに含まれるユーザープロファイル部分を `%USERPROFILE%` に置き換える。
pub fn mask_path(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    match std::env::var("USERPROFILE") {
        Ok(home) if !home.is_empty() => mask_with_home(&s, &home),
        _ => s,
    }
}

fn mask_with_home(s: &str, home: &str) -> String {
    let lower = s.to_lowercase();
    let home_lower = home.trim_end_matches('\\').to_lowercase();
    if let Some(idx) = lower.find(&home_lower) {
        let end = idx + home_lower.len();
        // 文字境界がずれる（大文字小文字でバイト長が変わる）ことは Windows のパスでは稀だが、念のため確認する。
        if s.is_char_boundary(idx) && s.is_char_boundary(end) {
            return format!("{}%USERPROFILE%{}", &s[..idx], &s[end..]);
        }
    }
    s.to_owned()
}

/// ファイル名だけを返す（ログでフルパスが不要な場面用）。
pub fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_user_profile_case_insensitively() {
        assert_eq!(
            mask_with_home(r"C:\Users\choko\Pictures\VRChat\a.png", r"C:\Users\choko"),
            r"%USERPROFILE%\Pictures\VRChat\a.png"
        );
        assert_eq!(
            mask_with_home(r"c:\users\CHOKO\x", r"C:\Users\choko\"),
            r"%USERPROFILE%\x"
        );
        assert_eq!(
            mask_with_home(r"D:\photos\a.png", r"C:\Users\choko"),
            r"D:\photos\a.png"
        );
    }

    #[test]
    fn file_name_only() {
        assert_eq!(file_name_of(Path::new(r"C:\a\b.png")), "b.png");
    }
}
