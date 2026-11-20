//! エクスプローラーの右クリック拡張（Windows 11 の最上位メニュー、`IExplorerCommand`）。
//!
//! 方針（INSTRUCTIONS §3.5）:
//! - 拡張はアップロード本体を持たない。選択されたパスを、同じ利用者だけが使える名前付きパイプで本体アプリへ渡すだけ。
//! - エクスプローラーを固めない: 重い処理・ネットワークはしない。パイプ通信・本体の起動は別スレッドで行う。
//! - エクスプローラーを巻き込まない: パニックを外へ出さない（すべての入口で捕まえて `E_FAIL` にする）。
//! - パスはここでは信頼せず、本体側で検証する（`cq_core::pathcheck`）。

pub mod menu;

#[cfg(windows)]
mod com;

#[cfg(windows)]
pub use com::{CLSID_SHARE_COMMAND, DllCanUnloadNow, DllGetClassObject};
