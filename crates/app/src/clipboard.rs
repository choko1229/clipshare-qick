//! クリップボード（Windows）。他のアプリが開いている間は失敗するので、少し待って再試行する。

use cq_core::clipboard::Clipboard;

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClipboard;

impl SystemClipboard {
    /// 文字列を入れる。成功したら `true`。
    pub fn set(&self, text: &str) -> bool {
        self.set_text(text).is_ok()
    }
}

impl Clipboard for SystemClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        let mut last = String::new();
        for attempt in 0..6 {
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_owned())) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = e.to_string();
                    std::thread::sleep(std::time::Duration::from_millis(40 * (attempt + 1)));
                }
            }
        }
        tracing::warn!(error = %last, "クリップボードに書き込めませんでした");
        Err(last)
    }
}
