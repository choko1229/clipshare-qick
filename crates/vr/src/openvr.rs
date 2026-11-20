//! OpenVR（`openvr_api.dll`）の動的読み込みと、C の関数テーブル（FnTable）の呼び出し。
//!
//! ビルド時に OpenVR SDK を必要としないよう、DLL を実行時に読み込み、`VR_GetGenericInterface("FnTable:...")`
//! で得た関数テーブルの位置（`openvr_capi.h` の構造体の並び）から関数を呼ぶ。
//! 使うインターフェースの版は固定する（IVROverlay_028 / IVRApplications_008）。

use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{HSTRING, PCSTR};

pub const IVROVERLAY_VERSION: &str = "IVROverlay_028";
pub const IVRAPPLICATIONS_VERSION: &str = "IVRApplications_008";

/// `openvr_capi.h` の `VR_IVROverlay_FnTable` の要素数と、使う関数の位置。
const OVERLAY_FN_COUNT: usize = 82;
mod ov {
    pub const FIND_OVERLAY: usize = 0;
    pub const CREATE_OVERLAY: usize = 1;
    pub const DESTROY_OVERLAY: usize = 3;
    pub const SET_OVERLAY_FLAG: usize = 11;
    pub const SET_OVERLAY_SORT_ORDER: usize = 20;
    pub const SET_OVERLAY_WIDTH_IN_METERS: usize = 22;
    pub const SET_OVERLAY_TRANSFORM_TRACKED_DEVICE_RELATIVE: usize = 35;
    pub const SHOW_OVERLAY: usize = 43;
    pub const HIDE_OVERLAY: usize = 44;
    pub const POLL_NEXT_OVERLAY_EVENT: usize = 48;
    pub const SET_OVERLAY_INPUT_METHOD: usize = 50;
    pub const SET_OVERLAY_MOUSE_SCALE: usize = 52;
    pub const SET_OVERLAY_RAW: usize = 62;
    pub const CREATE_DASHBOARD_OVERLAY: usize = 67;
}
/// `VR_IVRApplications_FnTable` の要素数と位置。
const APPS_FN_COUNT: usize = 31;
mod ap {
    pub const ADD_APPLICATION_MANIFEST: usize = 0;
    pub const REMOVE_APPLICATION_MANIFEST: usize = 1;
    pub const IS_APPLICATION_INSTALLED: usize = 2;
    pub const SET_APPLICATION_AUTO_LAUNCH: usize = 17;
}

/// `EVRApplicationType`
pub const APP_TYPE_OVERLAY: i32 = 2;
/// `VROverlayInputMethod_Mouse`
const INPUT_METHOD_MOUSE: i32 = 1;
/// `VROverlayFlags_SendVRDiscreteScrollEvents`
const FLAG_SEND_SCROLL: i32 = 64;
/// `k_unTrackedDeviceIndex_Hmd`
pub const HMD: u32 = 0;

/// イベント種別。
pub const EV_MOUSE_BUTTON_UP: u32 = 302;
pub const EV_OVERLAY_SHOWN: u32 = 500;
pub const EV_QUIT: u32 = 700;

pub type OverlayHandle = u64;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HmdMatrix34 {
    pub m: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HmdVector2 {
    pub v: [f32; 2],
}

/// `VREvent_t`（Windows 64bit では 64 バイト。data は 8 バイト境界の共用体 48 バイト）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VrEvent {
    pub event_type: u32,
    pub tracked_device_index: u32,
    pub event_age_seconds: f32,
    _pad: u32,
    pub data: [u64; 6],
}

impl Default for VrEvent {
    fn default() -> Self {
        Self {
            event_type: 0,
            tracked_device_index: 0,
            event_age_seconds: 0.0,
            _pad: 0,
            data: [0; 6],
        }
    }
}

impl VrEvent {
    /// マウスイベントの座標（`VREvent_Mouse_t` の x, y）。
    pub fn mouse(&self) -> (f32, f32) {
        let raw = self.data[0];
        (
            f32::from_bits(raw as u32),
            f32::from_bits((raw >> 32) as u32),
        )
    }
}

const _: () = assert!(std::mem::size_of::<VrEvent>() == 64);

#[derive(Debug, thiserror::Error)]
pub enum VrError {
    #[error("openvr_api.dll が見つかりません")]
    NoLibrary,
    #[error("OpenVR の関数が見つかりません")]
    MissingSymbol,
    #[error("SteamVR に接続できません（{0}）")]
    Init(i32),
    #[error("インターフェース {0} を取得できません")]
    Interface(&'static str),
    #[error("オーバーレイの操作に失敗しました（{0}）")]
    Overlay(i32),
    #[error("文字列が不正です")]
    BadString,
}

type InitInternal2 = unsafe extern "system" fn(*mut i32, i32, *const c_char) -> isize;
type InitInternal = unsafe extern "system" fn(*mut i32, i32) -> isize;
type Shutdown = unsafe extern "system" fn();
type GetGenericInterface = unsafe extern "system" fn(*const c_char, *mut i32) -> isize;

/// 接続中の OpenVR。ドロップで切断する。
#[derive(Debug)]
pub struct OpenVr {
    lib: HMODULE,
    shutdown: Shutdown,
    overlay: *const usize,
    apps: *const usize,
}

// SAFETY: 関数テーブルは読み取り専用で、OpenVR の関数は任意のスレッドから呼べる。
// ただしこのクレートでは 1 つのスレッドからだけ使う。
unsafe impl Send for OpenVr {}

fn cstr(s: &str) -> Result<CString, VrError> {
    CString::new(s).map_err(|_| VrError::BadString)
}

/// DLL の候補（実行ファイルと同じフォルダ → SteamVR のランタイム）。
pub fn library_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("openvr_api.dll"));
        }
    }
    if let Some(rt) = runtime_path() {
        v.push(rt.join("bin").join("win64").join("openvr_api.dll"));
    }
    v
}

/// `%LOCALAPPDATA%\openvr\openvrpaths.vrpath` からランタイムの場所を読む。
pub fn runtime_path() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
        .join("openvr")
        .join("openvrpaths.vrpath");
    parse_vrpath(&std::fs::read_to_string(p).ok()?)
}

pub fn parse_vrpath(text: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("runtime")?
        .as_array()?
        .first()?
        .as_str()
        .map(PathBuf::from)
}

unsafe fn sym<T: Copy>(lib: HMODULE, name: &[u8]) -> Option<T> {
    // SAFETY: name は NUL 終端。戻り値の関数ポインタを T（同じ大きさの関数ポインタ型）として扱う。
    unsafe {
        let p = GetProcAddress(lib, PCSTR(name.as_ptr()))?;
        Some(std::mem::transmute_copy::<_, T>(&p))
    }
}

impl OpenVr {
    /// オーバーレイアプリとして接続する。SteamVR が動いていないときは呼ばないこと（起動を促してしまうため）。
    pub fn connect(dll: &Path) -> Result<Self, VrError> {
        let wide = HSTRING::from(dll.as_os_str());
        // SAFETY: DLL を読み込む。失敗時は Err。
        let lib = unsafe { LoadLibraryW(&wide) }.map_err(|_| VrError::NoLibrary)?;
        let close = |e: VrError| {
            // SAFETY: 自分で読み込んだ DLL を解放する。
            let _ = unsafe { FreeLibrary(lib) };
            e
        };
        // SAFETY: シンボルは openvr_api.dll の公開関数で、型は openvr_capi.h に合わせている。
        let (init2, init, shutdown, get_iface) = unsafe {
            (
                sym::<InitInternal2>(lib, b"VR_InitInternal2\0"),
                sym::<InitInternal>(lib, b"VR_InitInternal\0"),
                sym::<Shutdown>(lib, b"VR_ShutdownInternal\0"),
                sym::<GetGenericInterface>(lib, b"VR_GetGenericInterface\0"),
            )
        };
        let (Some(shutdown), Some(get_iface)) = (shutdown, get_iface) else {
            return Err(close(VrError::MissingSymbol));
        };
        let mut err = 0i32;
        // SAFETY: err は書き込み先。起動情報は渡さない（null）。
        let _token = unsafe {
            match (init2, init) {
                (Some(f), _) => f(&mut err, APP_TYPE_OVERLAY, std::ptr::null()),
                (None, Some(f)) => f(&mut err, APP_TYPE_OVERLAY),
                (None, None) => return Err(close(VrError::MissingSymbol)),
            }
        };
        if err != 0 {
            return Err(close(VrError::Init(err)));
        }
        let iface = |name: &'static str| -> Result<*const usize, VrError> {
            let full = cstr(&format!("FnTable:{name}"))?;
            let mut e = 0i32;
            // SAFETY: インターフェース名は NUL 終端。戻り値は関数テーブルへのポインタ。
            let p = unsafe { get_iface(full.as_ptr(), &mut e) };
            if e != 0 || p == 0 {
                Err(VrError::Interface(name))
            } else {
                Ok(p as *const usize)
            }
        };
        let overlay = match iface(IVROVERLAY_VERSION) {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: 初期化に成功しているので切断する。
                unsafe { shutdown() };
                return Err(close(e));
            }
        };
        let apps = iface(IVRAPPLICATIONS_VERSION).unwrap_or(std::ptr::null());
        tracing::info!("SteamVR に接続しました");
        Ok(Self {
            lib,
            shutdown,
            overlay,
            apps,
        })
    }

    fn ov_fn(&self, index: usize) -> usize {
        debug_assert!(index < OVERLAY_FN_COUNT);
        // SAFETY: overlay は OVERLAY_FN_COUNT 個の関数ポインタの配列を指す（index は定数で範囲内）。
        unsafe { *self.overlay.add(index) }
    }

    fn ap_fn(&self, index: usize) -> Option<usize> {
        if self.apps.is_null() || index >= APPS_FN_COUNT {
            return None;
        }
        // SAFETY: apps は APPS_FN_COUNT 個の関数ポインタの配列を指す。
        Some(unsafe { *self.apps.add(index) })
    }

    fn check(code: i32) -> Result<(), VrError> {
        if code == 0 {
            Ok(())
        } else {
            Err(VrError::Overlay(code))
        }
    }

    pub fn find_or_create(&self, key: &str, name: &str) -> Result<OverlayHandle, VrError> {
        let k = cstr(key)?;
        let n = cstr(name)?;
        let mut h: OverlayHandle = 0;
        // SAFETY: 関数型は openvr_capi.h の宣言どおり。文字列と出力先は呼び出しの間有効。
        unsafe {
            let find: unsafe extern "system" fn(*const c_char, *mut u64) -> i32 =
                std::mem::transmute(self.ov_fn(ov::FIND_OVERLAY));
            if find(k.as_ptr(), &mut h) == 0 && h != 0 {
                return Ok(h);
            }
            let create: unsafe extern "system" fn(*const c_char, *const c_char, *mut u64) -> i32 =
                std::mem::transmute(self.ov_fn(ov::CREATE_OVERLAY));
            Self::check(create(k.as_ptr(), n.as_ptr(), &mut h))?;
        }
        Ok(h)
    }

    pub fn create_dashboard(
        &self,
        key: &str,
        name: &str,
    ) -> Result<(OverlayHandle, OverlayHandle), VrError> {
        let k = cstr(key)?;
        let n = cstr(name)?;
        let (mut main, mut thumb) = (0u64, 0u64);
        // SAFETY: 宣言どおりの型。出力先は呼び出しの間有効。
        unsafe {
            let f: unsafe extern "system" fn(
                *const c_char,
                *const c_char,
                *mut u64,
                *mut u64,
            ) -> i32 = std::mem::transmute(self.ov_fn(ov::CREATE_DASHBOARD_OVERLAY));
            Self::check(f(k.as_ptr(), n.as_ptr(), &mut main, &mut thumb))?;
        }
        Ok((main, thumb))
    }

    pub fn destroy(&self, h: OverlayHandle) {
        // SAFETY: 宣言どおりの型。
        unsafe {
            let f: unsafe extern "system" fn(u64) -> i32 =
                std::mem::transmute(self.ov_fn(ov::DESTROY_OVERLAY));
            let _ = f(h);
        }
    }

    pub fn set_width(&self, h: OverlayHandle, meters: f32) -> Result<(), VrError> {
        // SAFETY: 宣言どおりの型。
        unsafe {
            let f: unsafe extern "system" fn(u64, f32) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SET_OVERLAY_WIDTH_IN_METERS));
            Self::check(f(h, meters))
        }
    }

    pub fn set_sort_order(&self, h: OverlayHandle, order: u32) -> Result<(), VrError> {
        // SAFETY: 宣言どおりの型。
        unsafe {
            let f: unsafe extern "system" fn(u64, u32) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SET_OVERLAY_SORT_ORDER));
            Self::check(f(h, order))
        }
    }

    /// HMD からの相対位置に置く（視界内の通知）。
    pub fn attach_to_hmd(&self, h: OverlayHandle, x: f32, y: f32, z: f32) -> Result<(), VrError> {
        let m = HmdMatrix34 {
            m: [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, z]],
        };
        // SAFETY: 宣言どおりの型。行列は呼び出しの間有効。
        unsafe {
            let f: unsafe extern "system" fn(u64, u32, *const HmdMatrix34) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SET_OVERLAY_TRANSFORM_TRACKED_DEVICE_RELATIVE));
            Self::check(f(h, HMD, &m))
        }
    }

    pub fn show(&self, h: OverlayHandle) -> Result<(), VrError> {
        // SAFETY: 宣言どおりの型。
        unsafe {
            let f: unsafe extern "system" fn(u64) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SHOW_OVERLAY));
            Self::check(f(h))
        }
    }

    pub fn hide(&self, h: OverlayHandle) -> Result<(), VrError> {
        // SAFETY: 宣言どおりの型。
        unsafe {
            let f: unsafe extern "system" fn(u64) -> i32 =
                std::mem::transmute(self.ov_fn(ov::HIDE_OVERLAY));
            Self::check(f(h))
        }
    }

    /// RGBA の画像を設定する。
    pub fn set_raw(
        &self,
        h: OverlayHandle,
        rgba: &mut [u8],
        w: u32,
        hgt: u32,
    ) -> Result<(), VrError> {
        if rgba.len() != (w * hgt * 4) as usize {
            return Err(VrError::Overlay(-1));
        }
        // SAFETY: バッファは w*h*4 バイトで、呼び出しの間有効（SteamVR が内部にコピーする）。
        unsafe {
            let f: unsafe extern "system" fn(u64, *mut c_void, u32, u32, u32) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SET_OVERLAY_RAW));
            Self::check(f(h, rgba.as_mut_ptr().cast(), w, hgt, 4))
        }
    }

    /// マウス入力を受け付ける（ダッシュボードパネル）。座標の大きさは画像のピクセル数に合わせる。
    pub fn enable_mouse(&self, h: OverlayHandle, w: u32, hgt: u32) -> Result<(), VrError> {
        let scale = HmdVector2 {
            v: [w as f32, hgt as f32],
        };
        // SAFETY: 宣言どおりの型。
        unsafe {
            let method: unsafe extern "system" fn(u64, i32) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SET_OVERLAY_INPUT_METHOD));
            Self::check(method(h, INPUT_METHOD_MOUSE))?;
            let ms: unsafe extern "system" fn(u64, *const HmdVector2) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SET_OVERLAY_MOUSE_SCALE));
            Self::check(ms(h, &scale))?;
            let flag: unsafe extern "system" fn(u64, i32, bool) -> i32 =
                std::mem::transmute(self.ov_fn(ov::SET_OVERLAY_FLAG));
            let _ = flag(h, FLAG_SEND_SCROLL, true);
        }
        Ok(())
    }

    pub fn poll_event(&self, h: OverlayHandle) -> Option<VrEvent> {
        let mut ev = VrEvent::default();
        // SAFETY: ev は 64 バイトの VREvent_t。大きさを正しく渡す。
        let got = unsafe {
            let f: unsafe extern "system" fn(u64, *mut VrEvent, u32) -> bool =
                std::mem::transmute(self.ov_fn(ov::POLL_NEXT_OVERLAY_EVENT));
            f(h, &mut ev, std::mem::size_of::<VrEvent>() as u32)
        };
        got.then_some(ev)
    }

    /// `.vrmanifest` を登録し、SteamVR と一緒に起動する設定にする。
    pub fn register_manifest(&self, manifest: &Path, app_key: &str) -> Result<(), VrError> {
        let p = cstr(&manifest.to_string_lossy())?;
        let k = cstr(app_key)?;
        let (Some(add), Some(auto)) = (
            self.ap_fn(ap::ADD_APPLICATION_MANIFEST),
            self.ap_fn(ap::SET_APPLICATION_AUTO_LAUNCH),
        ) else {
            return Err(VrError::Interface(IVRAPPLICATIONS_VERSION));
        };
        // SAFETY: 宣言どおりの型。文字列は呼び出しの間有効。
        unsafe {
            let add: unsafe extern "system" fn(*const c_char, bool) -> i32 =
                std::mem::transmute(add);
            Self::check(add(p.as_ptr(), false))?;
            let auto: unsafe extern "system" fn(*const c_char, bool) -> i32 =
                std::mem::transmute(auto);
            Self::check(auto(k.as_ptr(), true))?;
        }
        Ok(())
    }

    pub fn unregister_manifest(&self, manifest: &Path) -> Result<(), VrError> {
        let p = cstr(&manifest.to_string_lossy())?;
        let Some(remove) = self.ap_fn(ap::REMOVE_APPLICATION_MANIFEST) else {
            return Err(VrError::Interface(IVRAPPLICATIONS_VERSION));
        };
        // SAFETY: 宣言どおりの型。
        unsafe {
            let remove: unsafe extern "system" fn(*const c_char) -> i32 =
                std::mem::transmute(remove);
            Self::check(remove(p.as_ptr()))
        }
    }

    pub fn is_installed(&self, app_key: &str) -> bool {
        let Ok(k) = cstr(app_key) else { return false };
        let Some(f) = self.ap_fn(ap::IS_APPLICATION_INSTALLED) else {
            return false;
        };
        // SAFETY: 宣言どおりの型。
        unsafe {
            let f: unsafe extern "system" fn(*const c_char) -> bool = std::mem::transmute(f);
            f(k.as_ptr())
        }
    }
}

impl Drop for OpenVr {
    fn drop(&mut self) {
        // SAFETY: 接続に成功したものだけがここに来る。切断してから DLL を解放する。
        unsafe {
            (self.shutdown)();
            let _ = FreeLibrary(self.lib);
        }
        tracing::info!("SteamVR から切断しました");
    }
}

/// SteamVR（vrserver.exe）が動いているか。
pub fn steamvr_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: スナップショットを列挙して閉じる。構造体の大きさは設定済み。
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = false;
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..len]);
                if name.eq_ignore_ascii_case("vrserver.exe") {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_layout_matches_header() {
        assert_eq!(std::mem::size_of::<VrEvent>(), 64);
        assert_eq!(std::mem::offset_of!(VrEvent, data), 16);
        let mut ev = VrEvent::default();
        ev.data[0] = u64::from(12.5f32.to_bits()) | (u64::from(300.0f32.to_bits()) << 32);
        assert_eq!(ev.mouse(), (12.5, 300.0));
    }

    #[test]
    fn vrpath_parsing() {
        let p =
            parse_vrpath(r#"{"runtime":["C:\\Steam\\steamapps\\common\\SteamVR"],"version":1}"#)
                .unwrap();
        assert!(p.ends_with("SteamVR"));
        assert!(parse_vrpath("{}").is_none());
    }

    #[test]
    fn missing_library_is_an_error() {
        assert!(matches!(
            OpenVr::connect(Path::new(r"C:\nope\openvr_api.dll")),
            Err(VrError::NoLibrary)
        ));
    }

    #[test]
    fn process_check_does_not_panic() {
        let _ = steamvr_running();
    }
}
