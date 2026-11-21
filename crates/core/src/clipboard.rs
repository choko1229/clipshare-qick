//! クリップボードの抽象（実装はアプリ側。テストではメモリ上の実装を使う）。

use std::sync::Mutex;

/// クリップボードへの書き込み。
pub trait Clipboard: Send + Sync + std::fmt::Debug {
    fn set_text(&self, text: &str) -> Result<(), String>;
}

/// 複数の URL を改行区切りでまとめる（右クリックの複数選択・連続撮影）。
pub fn join_urls<S: AsRef<str>>(urls: &[S]) -> String {
    urls.iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n")
}

/// テスト用。
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    pub text: Mutex<Option<String>>,
}

impl MemoryClipboard {
    pub fn get(&self) -> Option<String> {
        self.text.lock().ok().and_then(|g| g.clone())
    }
}

impl Clipboard for MemoryClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        let mut g = self.text.lock().map_err(|_| "lock".to_owned())?;
        *g = Some(text.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_with_newlines() {
        assert_eq!(join_urls(&["a", "b", "c"]), "a\nb\nc");
        assert_eq!(join_urls::<&str>(&[]), "");
        let c = MemoryClipboard::default();
        c.set_text("x").unwrap();
        assert_eq!(c.get().as_deref(), Some("x"));
    }
}
