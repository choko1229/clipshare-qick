//! トレイアイコンとメニュー（画面 08）。
//!
//! アイコンは状態ごとに描き分ける: 待機中 / アップロード中（回転）/ エラーあり（赤い点）/
//! 一時停止（グレー）/ 要ログイン（黄色の点）。

use std::sync::Arc;

use cq_core::engine::TrayState;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::app::{Command, Screen, SettingsTab, Shared};

const SIZE: u32 = 32;

/// 色（RGBA）。
type Rgba = [u8; 4];
const GREEN: Rgba = [0x48, 0xd6, 0xa8, 0xff];
const GRAY: Rgba = [0x8a, 0x90, 0xa0, 0xff];
const INK: Rgba = [0x06, 0x11, 0x0d, 0xff];
const RED: Rgba = [0xf1, 0x5d, 0x6a, 0xff];
const YELLOW: Rgba = [0xf5, 0xb8, 0x4a, 0xff];

fn put(buf: &mut [u8], size: u32, x: u32, y: u32, c: Rgba, alpha: f32) {
    if x >= size || y >= size {
        return;
    }
    let i = ((y * size + x) * 4) as usize;
    let Some(px) = buf.get_mut(i..i + 4) else {
        return;
    };
    let a = alpha.clamp(0.0, 1.0) * f32::from(c[3]) / 255.0;
    for k in 0..3 {
        px[k] = (f32::from(c[k]) * a + f32::from(px[k]) * (1.0 - a)) as u8;
    }
    px[3] = (a * 255.0 + f32::from(px[3]) * (1.0 - a)).min(255.0) as u8;
}

/// アイコンの画像を描く。`spin` はアップロード中の回転角（ラジアン、`None` なら静止）。
pub fn render(size: u32, base: Rgba, dot: Option<Rgba>, spin: Option<f32>) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let radius = s * 0.22;
    // 角丸の正方形（アンチエイリアスは 1px の簡易版）
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (radius - fx).max(fx - (s - radius)).max(0.0);
            let dy = (radius - fy).max(fy - (s - radius)).max(0.0);
            let d = (dx * dx + dy * dy).sqrt() - radius;
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                put(&mut buf, size, x, y, base, cover);
            }
        }
    }
    // 「C」の輪（右側が開いている）。回転時は開いている位置が回る。
    let (cx, cy) = (s / 2.0, s / 2.0);
    let (r_out, r_in) = (s * 0.30, s * 0.18);
    let gap_center = spin.unwrap_or(0.0);
    let gap_half = 0.75;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let r = (fx * fx + fy * fy).sqrt();
            let cover = ((r_out - r + 0.5).min(r - r_in + 0.5)).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let ang = fy.atan2(fx);
            let mut diff = (ang - gap_center).abs() % std::f32::consts::TAU;
            if diff > std::f32::consts::PI {
                diff = std::f32::consts::TAU - diff;
            }
            if diff > gap_half {
                put(&mut buf, size, x, y, INK, cover);
            }
        }
    }
    if let Some(c) = dot {
        let (dx, dy, dr) = (s * 0.80, s * 0.80, s * 0.18);
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5 - dx, y as f32 + 0.5 - dy);
                let r = (fx * fx + fy * fy).sqrt();
                let ring = (dr + 1.5 - r + 0.5).clamp(0.0, 1.0);
                if ring > 0.0 {
                    put(&mut buf, size, x, y, [0x07, 0x08, 0x0d, 0xff], ring);
                }
                let cover = (dr - r + 0.5).clamp(0.0, 1.0);
                if cover > 0.0 {
                    put(&mut buf, size, x, y, c, cover);
                }
            }
        }
    }
    buf
}

fn icon_for(state: TrayState, frame: u32) -> Option<tray_icon::Icon> {
    let (base, dot, spin) = match state {
        TrayState::Idle => (GREEN, None, None),
        TrayState::Uploading => (
            GREEN,
            None,
            Some(frame as f32 * std::f32::consts::TAU / 8.0),
        ),
        TrayState::Error | TrayState::Offline | TrayState::Blocked => (GREEN, Some(RED), None),
        TrayState::Paused => (GRAY, None, None),
        TrayState::NeedsLogin => (GREEN, Some(YELLOW), None),
    };
    tray_icon::Icon::from_rgba(render(SIZE, base, dot, spin), SIZE, SIZE).ok()
}

/// ウィンドウのアイコン。
pub fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: render(64, GREEN, None, None),
        width: 64,
        height: 64,
    }
}

/// 履歴パネルの位置（作業領域の右下、論理座標）。
pub fn panel_position(w: f32, h: f32) -> (f32, f32) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut rect = RECT::default();
    // SAFETY: RECT を受け取るだけの呼び出し。
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(std::ptr::from_mut(&mut rect).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    // SAFETY: 引数なしの API。
    let dpi = unsafe { GetDpiForSystem() }.max(96) as f32;
    let scale = dpi / 96.0;
    if !ok {
        return (100.0, 100.0);
    }
    let right = rect.right as f32 / scale;
    let bottom = rect.bottom as f32 / scale;
    ((right - w - 12.0).max(0.0), (bottom - h - 12.0).max(0.0))
}

/// トレイ。UI スレッドだけで使う。
pub struct Tray {
    icon: TrayIcon,
    auto_item: CheckMenuItem,
    state: Option<TrayState>,
    frame: u32,
    timer: usize,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray").finish_non_exhaustive()
    }
}

impl Tray {
    pub fn new(shared: &Arc<Shared>) -> Result<Self, String> {
        let open = MenuItem::new("履歴を開く", true, None);
        let auto_item = CheckMenuItem::new(
            "自動アップロード",
            true,
            shared.engine.settings().get().auto_upload_active(),
            None,
        );
        let settings = MenuItem::new("設定…", true, None);
        let web = MenuItem::new("Webで管理", true, None);
        let quit = MenuItem::new("終了", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &open,
            &auto_item,
            &PredefinedMenuItem::separator(),
            &settings,
            &web,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;

        let ids = (
            open.id().clone(),
            auto_item.id().clone(),
            settings.id().clone(),
            web.id().clone(),
            quit.id().clone(),
        );
        let s = Arc::clone(shared);
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let id = e.id();
            if *id == ids.0 {
                s.send(Command::Open(Screen::History));
            } else if *id == ids.1 {
                // メニューのチェックは押した時点で反転するため、結果に関係なく refresh で実際の状態に戻す。
                crate::actions::toggle_auto_upload(&s);
            } else if *id == ids.2 {
                s.send(Command::Open(Screen::Settings(SettingsTab::Upload)));
            } else if *id == ids.3 {
                s.send(Command::OpenUrl(crate::actions::manage_url(&s)));
            } else if *id == ids.4 {
                s.send(Command::Quit);
            }
        }));
        let s = Arc::clone(shared);
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                s.send(Command::Open(Screen::History));
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Clipshare Quick")
            .with_icon(icon_for(TrayState::Idle, 0).ok_or("icon")?)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            icon,
            auto_item,
            state: None,
            frame: 0,
            timer: 0,
        })
    }

    /// 状態に合わせてアイコンとメニューを更新する。
    pub fn refresh(&mut self, shared: &Arc<Shared>) {
        let snap = shared.engine.snapshot().borrow().clone();
        self.auto_item.set_checked(snap.auto_upload);
        // 403 の間は操作できない（Q-031）
        self.auto_item.set_enabled(!snap.blocked);
        let uploading = snap.tray == TrayState::Uploading;
        if uploading {
            self.frame = self.frame.wrapping_add(1);
            if self.timer == 0 {
                // SAFETY: スレッドのメッセージキューに WM_TIMER を送るタイマー（ウィンドウなし）。
                self.timer = unsafe { SetTimer(None, 0, 150, None) };
            }
        } else if self.timer != 0 {
            // SAFETY: SetTimer が返した ID を 1 回だけ止める。
            let _ = unsafe { KillTimer(None, self.timer) };
            self.timer = 0;
        }
        if self.state != Some(snap.tray) || uploading {
            if let Some(icon) = icon_for(snap.tray, self.frame) {
                let _ = self.icon.set_icon(Some(icon));
            }
            let tip = format!("Clipshare Quick — {}", snap.tray.label());
            let _ = self.icon.set_tooltip(Some(tip));
            self.state = Some(snap.tray);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.timer != 0 {
            // SAFETY: 動作中のタイマーを止める。
            let _ = unsafe { KillTimer(None, self.timer) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_render_for_every_state() {
        for s in [
            TrayState::Idle,
            TrayState::Uploading,
            TrayState::Error,
            TrayState::Paused,
            TrayState::NeedsLogin,
            TrayState::Offline,
        ] {
            assert!(icon_for(s, 3).is_some());
        }
        let px = render(32, GREEN, Some(RED), None);
        assert_eq!(px.len(), 32 * 32 * 4);
        // 角は透明、中央付近は不透明
        assert_eq!(px[3], 0);
        let center = ((16 * 32 + 4) * 4 + 3) as usize;
        assert_eq!(px[center], 255);
    }
}
