//! COM の実装（IExplorerCommand / IEnumExplorerCommand / IClassFactory と DLL の公開関数）。

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cq_ipc::{IpcError, Request, ShareQuality};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_FAIL, E_NOTIMPL, E_POINTER, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl,
    IShellItemArray, SHStrDupW, SIGDN_FILESYSPATH,
};
use windows_core::{
    BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PWSTR, Ref, Result, implement,
};

use crate::menu;

/// MSIX マニフェストに書く CLSID（`installer/AppxManifest.xml` と一致させる）。
pub const CLSID_SHARE_COMMAND: GUID = GUID::from_u128(0x6f3c2a8e_5b1d_4f7a_9c2e_8d4b1a7e3c51);
const CANON_ROOT: GUID = GUID::from_u128(0x6f3c2a8e_5b1d_4f7a_9c2e_8d4b1a7e3c52);
const CANON_ORIGINAL: GUID = GUID::from_u128(0x6f3c2a8e_5b1d_4f7a_9c2e_8d4b1a7e3c53);
const CANON_SHRINK: GUID = GUID::from_u128(0x6f3c2a8e_5b1d_4f7a_9c2e_8d4b1a7e3c54);

/// `EXPCMDSTATE`
const ECS_ENABLED: u32 = 0;
const ECS_HIDDEN: u32 = 2;
/// `EXPCMDFLAGS`
const ECF_DEFAULT: u32 = 0;
const ECF_HASSUBCOMMANDS: u32 = 1;

static DLL_MODULE: AtomicUsize = AtomicUsize::new(0);
static OBJECTS: AtomicUsize = AtomicUsize::new(0);

/// パニックを COM の失敗に変える（エクスプローラーを落とさないため）。
fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(E_FAIL.into()),
    }
}

fn dup(s: &str) -> Result<PWSTR> {
    // SAFETY: 文字列は呼び出しの間有効。返る PWSTR は CoTaskMemAlloc されたもので、呼び出し元（エクスプローラー）が解放する。
    unsafe { SHStrDupW(&HSTRING::from(s)) }
}

/// 選択されたファイルのパス。
fn paths_of(items: Option<&IShellItemArray>) -> Vec<PathBuf> {
    let Some(items) = items else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // SAFETY: IShellItemArray のメソッド呼び出し。返る文字列は CoTaskMemFree で解放する。
    unsafe {
        let Ok(n) = items.GetCount() else { return out };
        for i in 0..n.min(cq_ipc::MAX_SHARE_FILES as u32 * 2) {
            let Ok(item) = items.GetItemAt(i) else {
                continue;
            };
            if let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                if let Ok(s) = name.to_string() {
                    out.push(PathBuf::from(s));
                }
                CoTaskMemFree(Some(name.0 as *const c_void));
            }
        }
    }
    out
}

fn local_data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("ClipshareQuick"))
}

/// 利用者が設定で右クリックメニューを止めているか（フラグファイルの有無だけを見る。軽い）。
fn disabled_by_user() -> bool {
    local_data_dir().is_some_and(|d| d.join("contextmenu.disabled").exists())
}

fn default_quality() -> ShareQuality {
    local_data_dir()
        .and_then(|d| std::fs::read_to_string(d.join("settings.json")).ok())
        .map(|s| menu::default_quality_from_settings(&s))
        .unwrap_or(ShareQuality::Original)
}

/// 本体アプリの実行ファイル（実行エイリアス → DLL と同じフォルダ）。
fn app_exe() -> Option<PathBuf> {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let alias = PathBuf::from(local)
            .join("Microsoft")
            .join("WindowsApps")
            .join("clipshare-quick.exe");
        if alias.exists() {
            return Some(alias);
        }
    }
    let module = HMODULE(DLL_MODULE.load(Ordering::SeqCst) as *mut c_void);
    let mut buf = [0u16; 1024];
    // SAFETY: バッファの長さを渡している。module は DllMain で記録した自分の DLL。
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    let dll = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    Some(dll.parent()?.join("clipshare-quick.exe"))
}

/// 本体へ渡す（別スレッドで呼ぶ）。動いていなければ起動してから渡す。
fn deliver(paths: Vec<PathBuf>, quality: ShareQuality) {
    let req = Request::Share { quality, paths };
    match cq_ipc::send_request(&req, Duration::from_secs(2)) {
        Ok(_) => return,
        Err(IpcError::NotRunning) => {}
        Err(_) => return,
    }
    let Some(exe) = app_exe() else { return };
    if std::process::Command::new(exe)
        .arg("--background")
        .spawn()
        .is_err()
    {
        return;
    }
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(500));
        match cq_ipc::send_request(&req, Duration::from_secs(2)) {
            Ok(_) => return,
            Err(IpcError::NotRunning) => continue,
            Err(_) => return,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Root,
    Original,
    Shrink,
}

#[implement(IExplorerCommand)]
struct ShareCommand {
    kind: Kind,
}

impl ShareCommand {
    fn new(kind: Kind) -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self { kind }
    }
}

impl Drop for ShareCommand {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

impl IExplorerCommand_Impl for ShareCommand_Impl {
    fn GetTitle(&self, items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        guard(|| {
            let title = match self.kind {
                Kind::Root => menu::root_title(&menu::classify(&paths_of(items.as_ref()))),
                Kind::Original => menu::sub_title(ShareQuality::Original, default_quality()),
                Kind::Shrink => menu::sub_title(ShareQuality::Shrink, default_quality()),
            };
            dup(&title)
        })
    }

    fn GetIcon(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        guard(|| {
            let exe = app_exe().ok_or(windows_core::Error::from(E_NOTIMPL))?;
            dup(&format!("{},0", exe.display()))
        })
    }

    fn GetToolTip(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(match self.kind {
            Kind::Root => CANON_ROOT,
            Kind::Original => CANON_ORIGINAL,
            Kind::Shrink => CANON_SHRINK,
        })
    }

    fn GetState(&self, items: Ref<'_, IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guard(|| {
            let sel = menu::classify(&paths_of(items.as_ref()));
            let show = match self.kind {
                Kind::Root => menu::visible(&sel, disabled_by_user()),
                Kind::Original | Kind::Shrink => menu::has_submenu(&sel),
            };
            Ok(if show { ECS_ENABLED } else { ECS_HIDDEN })
        })
    }

    fn Invoke(&self, items: Ref<'_, IShellItemArray>, _bind: Ref<'_, IBindCtx>) -> Result<()> {
        guard(|| {
            let paths = menu::supported_paths(paths_of(items.as_ref()));
            if paths.is_empty() {
                return Ok(());
            }
            let quality = match self.kind {
                Kind::Root => ShareQuality::Default,
                Kind::Original => ShareQuality::Original,
                Kind::Shrink => ShareQuality::Shrink,
            };
            // エクスプローラーのスレッドを止めないよう、通信と起動は別スレッドで行う。
            let spawned = std::thread::Builder::new()
                .name("cq-shell-deliver".into())
                .spawn(move || {
                    let _ = catch_unwind(AssertUnwindSafe(|| deliver(paths, quality)));
                });
            spawned.map(|_| ()).map_err(|_| E_FAIL.into())
        })
    }

    fn GetFlags(&self) -> Result<u32> {
        // サブメニューの有無は選択内容に依存するが、GetFlags には選択が渡されない。
        // 最上位は常にサブコマンドを持ち、動画だけの選択では子を隠して（GetState）既定動作にする。
        Ok(match self.kind {
            Kind::Root => ECF_HASSUBCOMMANDS,
            Kind::Original | Kind::Shrink => ECF_DEFAULT,
        })
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        guard(|| {
            if self.kind != Kind::Root {
                return Err(E_NOTIMPL.into());
            }
            let items: Vec<IExplorerCommand> = vec![
                ShareCommand::new(Kind::Original).into(),
                ShareCommand::new(Kind::Shrink).into(),
            ];
            Ok(SubCommands {
                items,
                pos: AtomicUsize::new(0),
            }
            .into())
        })
    }
}

#[implement(IEnumExplorerCommand)]
struct SubCommands {
    items: Vec<IExplorerCommand>,
    pos: AtomicUsize,
}

impl IEnumExplorerCommand_Impl for SubCommands_Impl {
    fn Next(&self, celt: u32, out: *mut Option<IExplorerCommand>, fetched: *mut u32) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        let mut n = 0u32;
        while n < celt {
            let i = self.pos.load(Ordering::SeqCst);
            let Some(item) = self.items.get(i) else { break };
            // SAFETY: 呼び出し元は celt 個の要素を書ける配列を渡す（IEnumXXX の規約）。
            unsafe { out.add(n as usize).write(Some(item.clone())) };
            self.pos.store(i + 1, Ordering::SeqCst);
            n += 1;
        }
        if !fetched.is_null() {
            // SAFETY: null でなければ書き込み先として有効。
            unsafe { fetched.write(n) };
        }
        if n == celt { S_OK } else { S_FALSE }
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        self.pos.fetch_add(celt as usize, Ordering::SeqCst);
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.pos.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        Ok(SubCommands {
            items: self.items.clone(),
            pos: AtomicUsize::new(self.pos.load(Ordering::SeqCst)),
        }
        .into())
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> Result<()> {
        guard(|| {
            if ppv.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: ppv は null でない。失敗時に備えて先に null を書く。
            unsafe { ppv.write(std::ptr::null_mut()) };
            if outer.is_some() {
                return Err(windows::Win32::Foundation::CLASS_E_NOAGGREGATION.into());
            }
            let cmd: IExplorerCommand = ShareCommand::new(Kind::Root).into();
            // SAFETY: riid と ppv は呼び出し元が用意した有効なポインタ。
            unsafe { cmd.query(riid, ppv).ok() }
        })
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// DLL の読み込み時に自分のモジュールハンドルを覚える。
#[unsafe(no_mangle)]
extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        DLL_MODULE.store(module.0 as usize, Ordering::SeqCst);
    }
    true.into()
}

/// COM のクラスファクトリを返す。
///
/// # Safety
/// COM の規約どおり、`rclsid`・`riid`・`ppv` は有効なポインタであること。
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: null でないことは確認済み。
        unsafe {
            ppv.write(std::ptr::null_mut());
            if *rclsid != CLSID_SHARE_COMMAND {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            let factory: IClassFactory = Factory.into();
            factory.query(riid, ppv)
        }
    }));
    r.unwrap_or(E_FAIL)
}

/// 生きているオブジェクトが無ければアンロードしてよい。
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_factory_creates_command_and_rejects_other_clsid() {
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let other = GUID::from_u128(1);
        // SAFETY: テスト用に有効なポインタを渡す。
        let hr = unsafe { DllGetClassObject(&other, &IClassFactory::IID, &mut ppv) };
        assert_eq!(hr, CLASS_E_CLASSNOTAVAILABLE);
        // SAFETY: 同上。
        let hr = unsafe { DllGetClassObject(&CLSID_SHARE_COMMAND, &IClassFactory::IID, &mut ppv) };
        assert_eq!(hr, S_OK);
        // SAFETY: 成功したので ppv は IClassFactory。
        let factory = unsafe { IClassFactory::from_raw(ppv) };
        // SAFETY: CreateInstance の規約どおりに呼ぶ。
        let cmd: IExplorerCommand = unsafe { factory.CreateInstance(None) }.unwrap();
        // SAFETY: 選択なしで呼んでも失敗しないこと。
        let flags = unsafe { cmd.GetFlags() }.unwrap();
        assert_eq!(flags, ECF_HASSUBCOMMANDS);
        // SAFETY: 同上。
        let title = unsafe { cmd.GetTitle(None) }.unwrap();
        // SAFETY: SHStrDupW で確保された文字列を読んで解放する。
        let s = unsafe {
            let s = title.to_string().unwrap();
            CoTaskMemFree(Some(title.0 as *const c_void));
            s
        };
        assert_eq!(s, "Clipshareで共有");
        // SAFETY: サブコマンドの列挙。
        let e = unsafe { cmd.EnumSubCommands() }.unwrap();
        let mut out: [Option<IExplorerCommand>; 3] = [None, None, None];
        let mut fetched = 0u32;
        // SAFETY: 3 要素の配列に最大 3 個書かせる。
        let hr = unsafe { e.Next(&mut out, Some(&mut fetched)) };
        assert_eq!(hr, S_FALSE);
        assert_eq!(fetched, 2);
        // 選択が無ければメニューは隠れる
        // SAFETY: 選択なし。
        assert_eq!(unsafe { cmd.GetState(None, false) }.unwrap(), ECS_HIDDEN);
    }
}
