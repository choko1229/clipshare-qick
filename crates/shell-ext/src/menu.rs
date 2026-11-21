//! メニューの内容を決める純粋な処理（エクスプローラーに依存しないのでテストできる）。

use std::path::{Path, PathBuf};

use cq_ipc::ShareQuality;

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp"];
const VIDEO_EXT: &[&str] = &["mp4", "mov", "webm", "m4v", "mkv", "avi"];

/// 選択されたファイルの内訳。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub images: usize,
    pub videos: usize,
    pub other: usize,
}

impl Selection {
    pub fn supported(&self) -> usize {
        self.images + self.videos
    }
}

fn ext(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

pub fn classify(paths: &[PathBuf]) -> Selection {
    let mut s = Selection::default();
    for p in paths {
        let e = ext(p);
        if IMAGE_EXT.contains(&e.as_str()) {
            s.images += 1;
        } else if VIDEO_EXT.contains(&e.as_str()) {
            s.videos += 1;
        } else {
            s.other += 1;
        }
    }
    s
}

/// 対応しているファイルだけを残す（それ以外はアプリ側でも弾くが、送る量を減らす）。
pub fn supported_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|p| {
            let e = ext(p);
            IMAGE_EXT.contains(&e.as_str()) || VIDEO_EXT.contains(&e.as_str())
        })
        .take(cq_ipc::MAX_SHARE_FILES)
        .collect()
}

/// 最上位の項目名（画面 14）。
pub fn root_title(sel: &Selection) -> String {
    let n = sel.supported();
    if n > 1 {
        format!("Clipshareで共有（{n}件）")
    } else {
        "Clipshareで共有".to_owned()
    }
}

/// 画質のサブメニューを出すか（画像を含むときだけ。動画には画質の選択がない）。
pub fn has_submenu(sel: &Selection) -> bool {
    sel.images > 0
}

/// メニューを出すか。
pub fn visible(sel: &Selection, disabled_by_user: bool) -> bool {
    !disabled_by_user && sel.supported() > 0
}

/// サブメニューの項目名。既定の画質には「（既定）」を付ける。
pub fn sub_title(q: ShareQuality, default: ShareQuality) -> String {
    let base = match q {
        ShareQuality::Shrink => "縮小して共有",
        ShareQuality::Original | ShareQuality::Default => "原寸で共有",
    };
    if q == default {
        format!("{base}（既定）")
    } else {
        base.to_owned()
    }
}

/// アプリの設定（settings.json）から既定の画質を読む。読めなければアプリと同じ既定（縮小、Q-011）。
pub fn default_quality_from_settings(json: &str) -> ShareQuality {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return ShareQuality::Shrink,
    };
    match v.get("defaultQuality").and_then(|q| q.as_str()) {
        Some("original") => ShareQuality::Original,
        _ => ShareQuality::Shrink,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn titles_follow_design() {
        let one = classify(&p(&["IMG_0412.png"]));
        assert_eq!(root_title(&one), "Clipshareで共有");
        assert!(has_submenu(&one));
        let three = classify(&p(&["a.png", "b.jpg", "c.webp"]));
        assert_eq!(root_title(&three), "Clipshareで共有（3件）");
        let video = classify(&p(&["clip_0412.mp4"]));
        assert_eq!(root_title(&video), "Clipshareで共有");
        assert!(!has_submenu(&video));
        // 既定は縮小（Q-011）。「（既定）」は縮小側に付く
        assert_eq!(
            sub_title(ShareQuality::Shrink, ShareQuality::Shrink),
            "縮小して共有（既定）"
        );
        assert_eq!(
            sub_title(ShareQuality::Original, ShareQuality::Shrink),
            "原寸で共有"
        );
    }

    #[test]
    fn visibility() {
        assert!(!visible(&classify(&p(&["a.txt"])), false));
        assert!(!visible(&classify(&p(&["a.png"])), true));
        assert!(visible(&classify(&p(&["a.PNG", "x.txt"])), false));
        assert_eq!(
            supported_paths(p(&["a.png", "b.txt", "c.mov"])),
            p(&["a.png", "c.mov"])
        );
    }

    #[test]
    fn default_quality_is_read_from_settings() {
        assert_eq!(
            default_quality_from_settings(r#"{"defaultQuality":"shrink"}"#),
            ShareQuality::Shrink
        );
        assert_eq!(
            default_quality_from_settings(r#"{"defaultQuality":"original"}"#),
            ShareQuality::Original
        );
        assert_eq!(
            default_quality_from_settings("broken"),
            ShareQuality::Shrink
        );
        assert_eq!(default_quality_from_settings("{}"), ShareQuality::Shrink);
    }
}
