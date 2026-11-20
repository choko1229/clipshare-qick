//! SteamVR（OpenVR）連携: 視界内の HUD 通知（画面 18）とダッシュボードパネル（画面 19）。
//!
//! - SteamVR が動いていないときは何もしない（定期的に起動を確認するだけ。アプリ本体には影響しない）。
//! - 専用のスレッドで動き、アプリからは [`VrService`] の命令で操作する。
//! - QR コード・手首オーバーレイ・OSC は対象外。

pub mod canvas;
pub mod openvr;
pub mod view;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub use view::{HudMessage, HudTone, PanelAction, PanelItem, PanelModel, PanelStatus};

use openvr::{EV_MOUSE_BUTTON_UP, EV_QUIT, OpenVr, OverlayHandle};
use view::{Fonts, Hit, ThumbCache};

/// SteamVR 上のアプリキー。
pub const APP_KEY: &str = "choko1229.clipshare-quick";
const HUD_KEY: &str = "choko1229.clipshare-quick.hud";
const DASH_KEY: &str = "choko1229.clipshare-quick.dashboard";
const HUD_SECONDS: u64 = 3;

/// アプリからの命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VrCommand {
    Hud(HudMessage),
    Panel(PanelModel),
    /// SteamVR へのアプリ登録（SteamVR と一緒に起動）を設定する。
    SetRegistered(bool),
    Shutdown,
}

/// 設定。
#[derive(Debug, Clone)]
pub struct VrConfig {
    /// `.vrmanifest` を置くフォルダ。
    pub manifest_dir: PathBuf,
    /// SteamVR から起動するときの実行ファイル。
    pub exe_path: PathBuf,
    pub register: bool,
}

/// VR 連携のスレッド。
#[derive(Debug)]
pub struct VrService {
    tx: Sender<VrCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl VrService {
    /// 開始する。パネルでの操作は `on_action` で受け取る（VR スレッドから呼ばれる）。
    pub fn start(cfg: VrConfig, on_action: Box<dyn Fn(PanelAction) + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("cq-vr".into())
            .spawn(move || Worker::new(cfg, on_action).run(&rx))
            .ok();
        Self { tx, thread }
    }

    pub fn send(&self, c: VrCommand) {
        let _ = self.tx.send(c);
    }
}

impl Drop for VrService {
    fn drop(&mut self) {
        let _ = self.tx.send(VrCommand::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// `.vrmanifest` の内容。
pub fn manifest_json(exe: &Path) -> String {
    serde_json::json!({
        "source": "builtin",
        "applications": [{
            "app_key": APP_KEY,
            "launch_type": "binary",
            "binary_path_windows": exe.to_string_lossy(),
            "arguments": "--background",
            "is_dashboard_overlay": true,
            "strings": {
                "ja_jp": { "name": "Clipshare Quick", "description": "VRChatの写真を共有URLにします" },
                "en_us": { "name": "Clipshare Quick", "description": "Share VRChat photos as URLs" }
            }
        }]
    })
    .to_string()
}

struct Connected {
    vr: OpenVr,
    hud: OverlayHandle,
    dash: Option<OverlayHandle>,
}

struct Worker {
    cfg: VrConfig,
    on_action: Box<dyn Fn(PanelAction) + Send>,
    conn: Option<Connected>,
    fonts: Option<Fonts>,
    next_probe: Instant,
    hud_hide_at: Option<Instant>,
    model: PanelModel,
    hits: Vec<Hit>,
    thumbs: ThumbCache,
    panel_dirty: bool,
    registration_applied: Option<bool>,
}

impl Worker {
    fn new(cfg: VrConfig, on_action: Box<dyn Fn(PanelAction) + Send>) -> Self {
        Self {
            cfg,
            on_action,
            conn: None,
            fonts: None,
            next_probe: Instant::now(),
            hud_hide_at: None,
            model: PanelModel::default(),
            hits: Vec::new(),
            thumbs: ThumbCache::default(),
            panel_dirty: true,
            registration_applied: None,
        }
    }

    fn run(mut self, rx: &Receiver<VrCommand>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(VrCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(VrCommand::Hud(m)) => self.show_hud(&m),
                Ok(VrCommand::Panel(m)) => {
                    let confirm = self.model.confirm_delete.clone();
                    self.model = m;
                    // 確認中の項目がまだあれば確認を続ける
                    if confirm
                        .as_ref()
                        .is_some_and(|id| self.model.items.iter().any(|i| &i.id == id))
                    {
                        self.model.confirm_delete = confirm;
                    }
                    self.panel_dirty = true;
                }
                Ok(VrCommand::SetRegistered(b)) => {
                    self.cfg.register = b;
                    self.registration_applied = None;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
        }
        self.disconnect();
    }

    fn tick(&mut self) {
        if self.conn.is_none() {
            if Instant::now() < self.next_probe {
                return;
            }
            self.next_probe = Instant::now() + Duration::from_secs(5);
            if openvr::steamvr_running() {
                self.connect();
            }
            return;
        }
        self.apply_registration();
        if self.hud_hide_at.is_some_and(|t| Instant::now() >= t) {
            if let Some(c) = &self.conn {
                let _ = c.vr.hide(c.hud);
            }
            self.hud_hide_at = None;
        }
        if self.panel_dirty {
            self.render_panel();
        }
        self.poll_events();
    }

    fn connect(&mut self) {
        let Some(dll) = openvr::library_candidates()
            .into_iter()
            .find(|p| p.is_file())
        else {
            tracing::debug!("openvr_api.dll が見つからないため SteamVR 連携を行いません");
            return;
        };
        if self.fonts.is_none() {
            self.fonts = Fonts::load();
        }
        let vr = match OpenVr::connect(&dll) {
            Ok(v) => v,
            Err(e) => {
                tracing::info!(error = %e, "SteamVR に接続できませんでした");
                return;
            }
        };
        let hud = match vr.find_or_create(HUD_KEY, "Clipshare Quick") {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(error = %e, "HUD を作れませんでした");
                return;
            }
        };
        let _ = vr.set_width(hud, 0.42);
        let _ = vr.set_sort_order(hud, 10);
        let _ = vr.attach_to_hmd(hud, 0.0, -0.22, -1.1);
        let dash = match vr.create_dashboard(DASH_KEY, "Clipshare Quick") {
            Ok((main, _thumb)) => {
                let _ = vr.set_width(main, 2.2);
                let _ = vr.enable_mouse(main, view::PANEL_W, view::PANEL_H);
                Some(main)
            }
            Err(e) => {
                tracing::warn!(error = %e, "ダッシュボードパネルを作れませんでした");
                None
            }
        };
        self.conn = Some(Connected { vr, hud, dash });
        self.panel_dirty = true;
        self.registration_applied = None;
    }

    fn disconnect(&mut self) {
        if let Some(c) = self.conn.take() {
            c.vr.destroy(c.hud);
            if let Some(d) = c.dash {
                c.vr.destroy(d);
            }
        }
    }

    fn apply_registration(&mut self) {
        let Some(c) = &self.conn else { return };
        if self.registration_applied == Some(self.cfg.register) {
            return;
        }
        let manifest = self.cfg.manifest_dir.join("clipshare-quick.vrmanifest");
        let r = if self.cfg.register {
            std::fs::create_dir_all(&self.cfg.manifest_dir)
                .and_then(|()| std::fs::write(&manifest, manifest_json(&self.cfg.exe_path)))
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    c.vr.register_manifest(&manifest, APP_KEY)
                        .map_err(|e| e.to_string())
                })
        } else if c.vr.is_installed(APP_KEY) {
            c.vr.unregister_manifest(&manifest)
                .map_err(|e| e.to_string())
        } else {
            Ok(())
        };
        match r {
            Ok(()) => tracing::info!(
                registered = self.cfg.register,
                "SteamVR への登録を反映しました"
            ),
            Err(e) => tracing::warn!(error = %e, "SteamVR への登録を反映できませんでした"),
        }
        self.registration_applied = Some(self.cfg.register);
    }

    fn show_hud(&mut self, m: &HudMessage) {
        let (Some(c), Some(f)) = (&self.conn, &self.fonts) else {
            return;
        };
        let mut img = view::render_hud(f, m);
        if c.vr
            .set_raw(c.hud, &mut img.pixels, img.width, img.height)
            .is_ok()
        {
            let _ = c.vr.show(c.hud);
            self.hud_hide_at = Some(Instant::now() + Duration::from_secs(HUD_SECONDS));
        }
    }

    fn render_panel(&mut self) {
        self.panel_dirty = false;
        let (Some(c), Some(f)) = (&self.conn, &self.fonts) else {
            return;
        };
        let Some(dash) = c.dash else { return };
        let ids: Vec<&str> = self.model.items.iter().map(|i| i.id.as_str()).collect();
        self.thumbs.retain(&ids);
        let (mut img, hits) = view::render_panel(f, &self.model, &mut self.thumbs);
        if let Err(e) = c.vr.set_raw(dash, &mut img.pixels, img.width, img.height) {
            tracing::warn!(error = %e, "パネルを更新できませんでした");
        }
        self.hits = hits;
    }

    fn poll_events(&mut self) {
        let mut quit = false;
        let mut actions = Vec::new();
        if let Some(c) = &self.conn {
            while let Some(ev) = c.vr.poll_event(c.hud) {
                quit |= ev.event_type == EV_QUIT;
            }
            if let Some(d) = c.dash {
                while let Some(ev) = c.vr.poll_event(d) {
                    match ev.event_type {
                        EV_QUIT => quit = true,
                        EV_MOUSE_BUTTON_UP => {
                            let (x, y) = ev.mouse();
                            if let Some(a) = view::hit_test(&self.hits, x, y) {
                                actions.push(a);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for a in actions {
            match &a {
                PanelAction::AskDelete(id) => {
                    self.model.confirm_delete = Some(id.clone());
                    self.panel_dirty = true;
                }
                PanelAction::CancelDelete => {
                    self.model.confirm_delete = None;
                    self.panel_dirty = true;
                }
                PanelAction::ConfirmDelete(_) => {
                    self.model.confirm_delete = None;
                    self.panel_dirty = true;
                    (self.on_action)(a.clone());
                }
                PanelAction::ToggleAutoUpload | PanelAction::Retry(_) => {
                    (self.on_action)(a.clone())
                }
            }
        }
        if quit {
            tracing::info!("SteamVR が終了しました");
            self.disconnect();
            self.next_probe = Instant::now() + Duration::from_secs(10);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_contains_app_key_and_exe() {
        let j: serde_json::Value =
            serde_json::from_str(&manifest_json(Path::new(r"C:\x\clipshare-quick.exe"))).unwrap();
        assert_eq!(j["applications"][0]["app_key"], APP_KEY);
        assert_eq!(
            j["applications"][0]["binary_path_windows"],
            r"C:\x\clipshare-quick.exe"
        );
    }

    #[test]
    fn service_starts_and_stops_without_steamvr() {
        let dir = tempfile::tempdir().unwrap();
        let s = VrService::start(
            VrConfig {
                manifest_dir: dir.path().to_path_buf(),
                exe_path: PathBuf::from("x.exe"),
                register: false,
            },
            Box::new(|_| {}),
        );
        s.send(VrCommand::Hud(HudMessage {
            title: "t".into(),
            subtitle: "s".into(),
            tone: HudTone::Info,
        }));
        drop(s);
    }
}
