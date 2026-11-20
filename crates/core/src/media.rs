//! ファイル種別の判定と、サイズ超過時の画像縮小。

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 共有できるファイルの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum MediaKind {
    Image,
    Video,
}

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp"];
const VIDEO_EXT: &[&str] = &["mp4", "mov", "webm", "m4v"];

/// サイズ超過時の縮小で使う長辺の長さ（サーバーの「縮小」と同じ）。
pub const SHRINK_LONG_EDGE: u32 = 2000;

impl MediaKind {
    /// 拡張子から種別を判定する。
    pub fn from_path(p: &Path) -> Option<Self> {
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        if IMAGE_EXT.contains(&ext.as_str()) {
            Some(Self::Image)
        } else if VIDEO_EXT.contains(&ext.as_str()) {
            Some(Self::Video)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "画像",
            Self::Video => "動画",
        }
    }
}

/// 送信時の MIME タイプ（拡張子から）。
pub fn mime_for(p: &Path) -> &'static str {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// 先頭バイトから中身の種別を推定する（拡張子と食い違うファイルを送らないため）。
pub fn sniff(head: &[u8]) -> Option<MediaKind> {
    let starts = |sig: &[u8]| head.len() >= sig.len() && &head[..sig.len()] == sig;
    if starts(b"\x89PNG\r\n\x1a\n") || starts(b"\xFF\xD8\xFF") {
        return Some(MediaKind::Image);
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some(MediaKind::Image);
    }
    // ISO BMFF（mp4/mov/m4v）: 4..8 が "ftyp"。古い mov は moov/mdat/wide/free で始まることがある。
    if head.len() >= 8 {
        let t = &head[4..8];
        if t == b"ftyp" || t == b"moov" || t == b"mdat" || t == b"wide" || t == b"free" {
            return Some(MediaKind::Video);
        }
    }
    if starts(b"\x1A\x45\xDF\xA3") {
        return Some(MediaKind::Video); // WebM (EBML)
    }
    None
}

/// ファイルの先頭を読み、拡張子と中身が一致するか確認する。
pub fn verify_file_kind(p: &Path, expected: MediaKind) -> std::io::Result<bool> {
    let mut f = std::fs::File::open(p)?;
    let mut head = [0u8; 32];
    let mut n = 0;
    while n < head.len() {
        let r = f.read(&mut head[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(sniff(&head[..n]) == Some(expected))
}

/// 縮小の失敗。
#[derive(Debug, thiserror::Error)]
pub enum ShrinkError {
    #[error("画像を読み込めませんでした")]
    Decode(#[source] image::ImageError),
    #[error("画像を変換できませんでした")]
    Encode(#[source] image::ImageError),
    #[error("ファイルを読み込めませんでした")]
    Io(#[source] std::io::Error),
}

/// 縮小した画像。
#[derive(Debug)]
pub struct ShrunkImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
}

/// 長辺を `long_edge` 以下に縮小して再エンコードする。
///
/// 回転情報（EXIF Orientation）は反映してから縮小する。再エンコードにより撮影情報などの
/// 埋め込みデータは残らない。PNG/WebP は PNG、JPEG は JPEG で出力する。
pub fn shrink_image(p: &Path, long_edge: u32) -> Result<ShrunkImage, ShrinkError> {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(p)
        .map_err(ShrinkError::Io)?
        .with_guessed_format()
        .map_err(ShrinkError::Io)?;
    let format = reader.format();
    let mut decoder = reader.into_decoder().map_err(ShrinkError::Decode)?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(ShrinkError::Decode)?;
    img.apply_orientation(orientation);

    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > long_edge {
        img.resize(long_edge, long_edge, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_owned());
    let mut bytes = Vec::new();
    let (mime, ext) = if format == Some(image::ImageFormat::Jpeg) {
        let rgb = img.to_rgb8();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 92);
        enc.encode_image(&rgb).map_err(ShrinkError::Encode)?;
        ("image/jpeg", "jpg")
    } else {
        img.write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .map_err(ShrinkError::Encode)?;
        ("image/png", "png")
    };
    Ok(ShrunkImage {
        width: img.width(),
        height: img.height(),
        bytes,
        mime,
        file_name: format!("{stem}.{ext}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_extension() {
        assert_eq!(
            MediaKind::from_path(Path::new("a.PNG")),
            Some(MediaKind::Image)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("a.jpeg")),
            Some(MediaKind::Image)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("a.mp4")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("a.MOV")),
            Some(MediaKind::Video)
        );
        assert_eq!(MediaKind::from_path(Path::new("a.txt")), None);
        assert_eq!(MediaKind::from_path(Path::new("noext")), None);
    }

    #[test]
    fn sniff_signatures() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(MediaKind::Image));
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0"), Some(MediaKind::Image));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(MediaKind::Image));
        assert_eq!(sniff(b"\0\0\0\x20ftypisom"), Some(MediaKind::Video));
        assert_eq!(sniff(b"\x1A\x45\xDF\xA3\x01"), Some(MediaKind::Video));
        assert_eq!(sniff(b"hello world"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn shrink_reduces_long_edge_and_keeps_aspect() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.png");
        let img = image::RgbImage::from_fn(3000, 1500, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 7])
        });
        img.save(&p).unwrap();
        let out = shrink_image(&p, SHRINK_LONG_EDGE).unwrap();
        assert_eq!((out.width, out.height), (2000, 1000));
        assert_eq!(out.mime, "image/png");
        assert_eq!(out.file_name, "big.png");
        let decoded = image::load_from_memory(&out.bytes).unwrap();
        assert_eq!(decoded.width(), 2000);
    }

    #[test]
    fn shrink_jpeg_stays_jpeg_and_small_images_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jpg");
        let img = image::RgbImage::from_pixel(100, 50, image::Rgb([1, 2, 3]));
        img.save(&p).unwrap();
        let out = shrink_image(&p, SHRINK_LONG_EDGE).unwrap();
        assert_eq!((out.width, out.height), (100, 50));
        assert_eq!(out.mime, "image/jpeg");
        assert!(verify_file_kind(&p, MediaKind::Image).unwrap());
        assert!(!verify_file_kind(&p, MediaKind::Video).unwrap());
    }

    #[test]
    fn shrink_rejects_non_image() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fake.png");
        std::fs::write(&p, b"not an image").unwrap();
        assert!(shrink_image(&p, SHRINK_LONG_EDGE).is_err());
    }
}
