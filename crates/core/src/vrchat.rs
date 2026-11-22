//! VRChat の写真フォルダの検出。
//!
//! 1. `%USERPROFILE%\AppData\LocalLow\VRChat\VRChat\config.json` の `picture_output_folder` を読む（存在するフォルダなら優先）。
//! 2. 無ければ既定の「ピクチャ」フォルダ配下の `VRChat`。
//! 3. どちらも無ければ検出できない（フォルダは作らない。画面で手動選択してもらう）。

use std::path::{Path, PathBuf};

/// 検出結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// config.json の設定から見つけた。
    FromConfig(PathBuf),
    /// 既定の場所にあった。
    Default(PathBuf),
    NotFound,
}

impl Detection {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::FromConfig(p) | Self::Default(p) => Some(p),
            Self::NotFound => None,
        }
    }
}

/// VRChat の config.json の場所。
pub fn config_path(user_profile: &Path) -> PathBuf {
    user_profile
        .join("AppData")
        .join("LocalLow")
        .join("VRChat")
        .join("VRChat")
        .join("config.json")
}

/// config.json の内容から写真の出力先を取り出す。
pub fn parse_picture_folder(json: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let s = v.get("picture_output_folder")?.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    let p = PathBuf::from(s);
    crate::pathcheck::is_local_absolute(&p).then_some(p)
}

/// 検出する。`pictures` は「ピクチャ」フォルダ（既知のフォルダ。OneDrive へ移動されている場合もある）。
pub fn detect(user_profile: &Path, pictures: &Path) -> Detection {
    let cfg = config_path(user_profile);
    match std::fs::read_to_string(&cfg) {
        Ok(text) => match parse_picture_folder(&text) {
            Some(p) if p.is_dir() => {
                tracing::info!(folder = %crate::logging::mask_path(&p), "VRChat の config.json から写真フォルダを検出しました");
                return Detection::FromConfig(p);
            }
            Some(p) => {
                tracing::warn!(folder = %crate::logging::mask_path(&p), "config.json の写真フォルダが存在しません");
            }
            None => tracing::debug!("config.json に写真フォルダの指定はありません"),
        },
        Err(e) => tracing::debug!(error = %e, "VRChat の config.json を読めませんでした"),
    }
    let default = pictures.join("VRChat");
    if default.is_dir() {
        Detection::Default(default)
    } else {
        Detection::NotFound
    }
}

/// 実行中の環境で検出する。
pub fn detect_current() -> Detection {
    let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) else {
        return Detection::NotFound;
    };
    let pictures = pictures_folder().unwrap_or_else(|| profile.join("Pictures"));
    detect(&profile, &pictures)
}

/// 「ピクチャ」の既知のフォルダ。
#[cfg(windows)]
pub fn pictures_folder() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: 成功時に返る文字列は CoTaskMemAlloc されたもので、読んだ後に解放する。
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

#[cfg(not(windows))]
pub fn pictures_folder() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config() {
        assert_eq!(
            parse_picture_folder(r#"{"picture_output_folder":"D:\\Photos\\VRC","other":1}"#),
            Some(PathBuf::from(r"D:\Photos\VRC"))
        );
        assert_eq!(
            parse_picture_folder(r#"{"picture_output_folder":""}"#),
            None
        );
        assert_eq!(
            parse_picture_folder(r#"{"picture_output_folder":"\\\\nas\\x"}"#),
            None
        );
        assert_eq!(parse_picture_folder(r#"{"cache_size":30}"#), None);
        assert_eq!(parse_picture_folder("broken"), None);
    }

    #[test]
    fn detection_order() {
        let home = tempfile::tempdir().unwrap();
        let pics = home.path().join("Pictures");
        assert_eq!(detect(home.path(), &pics), Detection::NotFound);

        std::fs::create_dir_all(pics.join("VRChat")).unwrap();
        assert_eq!(
            detect(home.path(), &pics),
            Detection::Default(pics.join("VRChat"))
        );

        let custom = home.path().join("custom");
        std::fs::create_dir_all(&custom).unwrap();
        let cfg = config_path(home.path());
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &cfg,
            serde_json::json!({ "picture_output_folder": custom }).to_string(),
        )
        .unwrap();
        assert_eq!(
            detect(home.path(), &pics),
            Detection::FromConfig(custom.clone())
        );

        // 設定先が存在しなければ既定へ
        std::fs::remove_dir(&custom).unwrap();
        assert_eq!(
            detect(home.path(), &pics),
            Detection::Default(pics.join("VRChat"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn pictures_known_folder_resolves() {
        assert!(pictures_folder().is_some());
    }
}
