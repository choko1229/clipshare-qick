//! 本体アプリとエクスプローラー拡張・2 つ目の起動プロセスとの間の IPC（名前付きパイプ）。
//!
//! - パイプ名は利用者の SID を含み、DACL で同じ利用者（と SYSTEM）だけに接続を許可する。
//!   リモートからの接続は拒否する（サーバー側で `PIPE_REJECT_REMOTE_CLIENTS`）。
//! - クライアントは接続後にサーバープロセスの所有者が自分と同じか確認する（なりすまし対策）。
//! - メッセージは 1 行の JSON（最大 [`MAX_MESSAGE_BYTES`]）。
//!
//! このクレートは非同期ランタイムに依存しない（エクスプローラー拡張 DLL から使うため）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// プロトコルのバージョン。
pub const PROTOCOL_VERSION: u32 = 1;
/// 1 メッセージの最大バイト数。
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// 1 回の共有で受け付ける最大ファイル数。
pub const MAX_SHARE_FILES: usize = 500;

/// 共有時の画質の指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareQuality {
    /// 設定の既定値を使う。
    Default,
    Original,
    Shrink,
}

/// 開く画面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    History,
    Settings,
}

/// 要求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Request {
    /// ファイルを共有する（右クリックから）。
    Share {
        quality: ShareQuality,
        paths: Vec<PathBuf>,
    },
    /// 画面を開く（2 つ目の起動・通知のクリックから）。
    Show { view: View },
}

/// 応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Response {
    Accepted,
    Rejected { reason: String },
}

/// 送受信の封筒（バージョンつき）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub v: u32,
    #[serde(flatten)]
    pub body: T,
}

/// IPC の失敗。
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("Clipshare Quick が起動していません")]
    NotRunning,
    #[error("パイプの相手が信頼できません")]
    UntrustedPeer,
    #[error("メッセージが大きすぎます")]
    TooLarge,
    #[error("メッセージの形式が不正です")]
    Malformed,
    #[error("バージョンが一致しません")]
    VersionMismatch,
    #[error("通信に失敗しました")]
    Io(#[from] std::io::Error),
    #[error("OS の呼び出しに失敗しました（0x{0:08X}）")]
    Os(i32),
}

/// 1 行の JSON にする（末尾に改行）。
pub fn encode<T: Serialize>(body: T) -> Result<Vec<u8>, IpcError> {
    let mut v = serde_json::to_vec(&Envelope {
        v: PROTOCOL_VERSION,
        body,
    })
    .map_err(|_| IpcError::Malformed)?;
    if v.len() + 1 > MAX_MESSAGE_BYTES {
        return Err(IpcError::TooLarge);
    }
    v.push(b'\n');
    Ok(v)
}

/// 1 行の JSON を読む。
pub fn decode<T: for<'de> Deserialize<'de>>(line: &[u8]) -> Result<T, IpcError> {
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(IpcError::TooLarge);
    }
    let env: Envelope<T> = serde_json::from_slice(line).map_err(|_| IpcError::Malformed)?;
    if env.v != PROTOCOL_VERSION {
        return Err(IpcError::VersionMismatch);
    }
    Ok(env.body)
}

/// `reader` から改行までを上限つきで読む。
pub fn read_line_limited<R: std::io::Read>(reader: &mut R) -> Result<Vec<u8>, IpcError> {
    let mut out = Vec::with_capacity(4096);
    let mut buf = [0u8; 4096];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = buf.get(..n).ok_or(IpcError::Malformed)?;
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            out.extend_from_slice(chunk.get(..pos).ok_or(IpcError::Malformed)?);
            return Ok(out);
        }
        out.extend_from_slice(chunk);
        if out.len() > MAX_MESSAGE_BYTES {
            return Err(IpcError::TooLarge);
        }
    }
    if out.is_empty() {
        Err(IpcError::Malformed)
    } else {
        Ok(out)
    }
}

#[cfg(windows)]
pub mod win;

#[cfg(windows)]
pub use win::{PipeSecurity, current_user_sid, pipe_name, send_request};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_version_check() {
        let req = Request::Share {
            quality: ShareQuality::Shrink,
            paths: vec![PathBuf::from(r"C:\a.png")],
        };
        let bytes = encode(&req).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let line = &bytes[..bytes.len() - 1];
        let back: Request = decode(line).unwrap();
        assert_eq!(back, req);
        let s = String::from_utf8(line.to_vec()).unwrap();
        assert!(s.contains("\"v\":1"));
        assert!(s.contains("\"type\":\"share\""));

        let wrong = br#"{"v":99,"type":"accepted"}"#;
        assert!(matches!(
            decode::<Response>(wrong),
            Err(IpcError::VersionMismatch)
        ));
        assert!(matches!(
            decode::<Response>(b"nope"),
            Err(IpcError::Malformed)
        ));
    }

    #[test]
    fn line_reader_limits() {
        let mut r: &[u8] = b"{\"a\":1}\nrest";
        assert_eq!(read_line_limited(&mut r).unwrap(), b"{\"a\":1}");
        let big = vec![b'x'; MAX_MESSAGE_BYTES + 10];
        let mut r: &[u8] = &big;
        assert!(matches!(read_line_limited(&mut r), Err(IpcError::TooLarge)));
        let mut r: &[u8] = b"";
        assert!(read_line_limited(&mut r).is_err());
    }

    #[test]
    fn too_large_message_is_rejected() {
        let req = Request::Share {
            quality: ShareQuality::Default,
            paths: (0..40_000)
                .map(|i| PathBuf::from(format!(r"C:\very\long\path\{i:08}.png")))
                .collect(),
        };
        assert!(matches!(encode(&req), Err(IpcError::TooLarge)));
    }
}
