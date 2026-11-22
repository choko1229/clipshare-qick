//! セットアップに自己署名の公開証明書（.cer）を埋め込む。
//!
//! 配布物はセットアップ exe と MSIX の 2 ファイル（Q-008）。証明書の信頼手順（画面 02 ④）で使う
//! 公開鍵のみの証明書を、ビルド時に環境変数 `CQ_SIGNING_CER` で指定したファイルから埋め込む。
//! 指定が無い場合は空（セットアップは同じフォルダの .cer を探す。開発用）。
//! 秘密鍵（.pfx）はここでは扱わない。

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=CQ_SIGNING_CER");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("signing.cer");
    let bytes = match std::env::var_os("CQ_SIGNING_CER") {
        Some(p) if !p.is_empty() => {
            let path = PathBuf::from(&p);
            println!("cargo:rerun-if-changed={}", path.display());
            match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    println!("cargo:warning=CQ_SIGNING_CER を読めません: {e}");
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };
    if let Err(e) = std::fs::write(&out, bytes) {
        println!("cargo:warning=証明書を書き出せません: {e}");
    }
}
