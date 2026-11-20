//! 外部から渡されたパス（右クリック拡張・コマンドライン・設定）の検証。
//!
//! 受け取ったパスは信頼しない。ローカルドライブの絶対パスで、通常ファイル（シンボリックリンクや
//! ジャンクションなどの再解析ポイントではない）で、対応する拡張子のものだけを通す。

use std::path::{Component, Path, PathBuf, Prefix};

use crate::media::MediaKind;

/// パス検証の失敗理由。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathRejected {
    #[error("ローカルの絶対パスではありません")]
    NotLocalAbsolute,
    #[error("ネットワーク上のファイルは共有できません")]
    Unc,
    #[error("親フォルダへの参照を含むパスは使えません")]
    Traversal,
    #[error("ファイルが見つかりません")]
    NotFound,
    #[error("通常のファイルではありません")]
    NotRegularFile,
    #[error("リンクやショートカット先のファイルは共有できません")]
    ReparsePoint,
    #[error("対応していないファイル形式です")]
    Unsupported,
}

/// `C:\...` 形式（ドライブ文字つき絶対パス）かどうか。UNC や `\\?\UNC\`、デバイスパスは拒否する。
pub fn is_local_absolute(p: &Path) -> bool {
    let mut comps = p.components();
    let ok_prefix = match comps.next() {
        Some(Component::Prefix(pre)) => {
            matches!(pre.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        }
        _ => false,
    };
    ok_prefix && matches!(comps.next(), Some(Component::RootDir))
}

fn is_unc(p: &Path) -> bool {
    matches!(
        p.components().next(),
        Some(Component::Prefix(pre)) if matches!(pre.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(_) | Prefix::Verbatim(_))
    )
}

/// 共有対象として受け取ったパスを検証し、正規化したパスと種別を返す。
pub fn validate_share_path(p: &Path) -> Result<(PathBuf, MediaKind), PathRejected> {
    if is_unc(p) {
        return Err(PathRejected::Unc);
    }
    if !is_local_absolute(p) {
        return Err(PathRejected::NotLocalAbsolute);
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(PathRejected::Traversal);
    }
    let meta = std::fs::symlink_metadata(p).map_err(|_| PathRejected::NotFound)?;
    if is_reparse_point(&meta) || meta.file_type().is_symlink() {
        return Err(PathRejected::ReparsePoint);
    }
    if !meta.is_file() {
        return Err(PathRejected::NotRegularFile);
    }
    let kind = MediaKind::from_path(p).ok_or(PathRejected::Unsupported)?;
    // 親フォルダ側にジャンクションがあっても、最終的な実体がネットワーク上なら拒否する。
    let canonical = std::fs::canonicalize(p).map_err(|_| PathRejected::NotFound)?;
    if is_unc(&strip_verbatim(&canonical)) {
        return Err(PathRejected::Unc);
    }
    Ok((strip_verbatim(&canonical), kind))
}

/// `\\?\C:\...` を `C:\...` に戻す（表示とログ用）。
pub fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn local_absolute_detection() {
        assert!(is_local_absolute(Path::new(r"C:\Users\a\Pictures")));
        assert!(is_local_absolute(Path::new(r"\\?\C:\x")));
        assert!(!is_local_absolute(Path::new(r"\\server\share\x")));
        assert!(!is_local_absolute(Path::new(r"\\?\UNC\server\share")));
        assert!(!is_local_absolute(Path::new(r"relative\x")));
        assert!(!is_local_absolute(Path::new(r"C:relative")));
        assert!(!is_local_absolute(Path::new(r"\\.\pipe\x")));
    }

    #[test]
    fn share_path_validation() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("a.png");
        std::fs::write(&img, b"x").unwrap();
        let (p, kind) = validate_share_path(&img).unwrap();
        assert_eq!(kind, MediaKind::Image);
        assert!(p.ends_with("a.png"));

        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, b"x").unwrap();
        assert_eq!(validate_share_path(&txt), Err(PathRejected::Unsupported));
        assert_eq!(
            validate_share_path(&dir.path().join("none.png")),
            Err(PathRejected::NotFound)
        );
        assert_eq!(
            validate_share_path(Path::new(r"\\server\share\a.png")),
            Err(PathRejected::Unc)
        );
        assert_eq!(
            validate_share_path(Path::new("a.png")),
            Err(PathRejected::NotLocalAbsolute)
        );
        let trav = dir.path().join("sub").join("..").join("a.png");
        assert_eq!(validate_share_path(&trav), Err(PathRejected::Traversal));
        let folder = dir.path().join("d.png");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(
            validate_share_path(&folder),
            Err(PathRejected::NotRegularFile)
        );
    }

    #[test]
    fn symlink_is_rejected_when_creatable() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("a.png");
        std::fs::write(&img, b"x").unwrap();
        let link = dir.path().join("l.png");
        // シンボリックリンクの作成には開発者モードか管理者権限が要る。作れない環境では確認を省く。
        if std::os::windows::fs::symlink_file(&img, &link).is_ok() {
            assert_eq!(validate_share_path(&link), Err(PathRejected::ReparsePoint));
        }
    }
}
