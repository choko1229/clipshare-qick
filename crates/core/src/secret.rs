//! 秘密値（トークンなど）をログやデバッグ出力に出さないための型。

use std::fmt;

use zeroize::Zeroizing;

/// 秘密の文字列。`Debug`/`Display` では中身を出さず、破棄時にメモリを消去する。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }

    /// 中身を取り出す。HTTP ヘッダや資格情報マネージャーに渡す直前だけ使うこと。
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(***)")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl serde::Serialize for SecretString {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.expose())
    }
}

impl<'de> serde::Deserialize<'de> for SecretString {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_hide_value() {
        let s = SecretString::new("cq_live_abcdef");
        assert_eq!(format!("{s:?}"), "SecretString(***)");
        assert_eq!(format!("{s}"), "***");
        assert_eq!(s.expose(), "cq_live_abcdef");
    }
}
