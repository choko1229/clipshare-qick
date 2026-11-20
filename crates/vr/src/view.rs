//! VR 内の表示（画面 18 の HUD 通知と、画面 19 のダッシュボードパネル）。
//!
//! 描画と当たり判定だけを持ち、SteamVR には依存しない（単体テストできる）。
//! VR 内にはログ・トークン・ファイルのパスを出さない（時刻・画質・状態だけ）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ab_glyph::FontVec;

use crate::canvas::{Canvas, Rgba, measure};

const BG: Rgba = [0x07, 0x08, 0x0d, 0xf0];
const CARD: Rgba = [0x10, 0x13, 0x1b, 0xff];
const BORDER: Rgba = [0x25, 0x2a, 0x38, 0xff];
const FG: Rgba = [0xf7, 0xf7, 0xfb, 0xff];
const MUTED: Rgba = [0xa3, 0xa8, 0xb8, 0xff];
const MUT_BG: Rgba = [0x15, 0x18, 0x21, 0xff];
const PRIMARY: Rgba = [0x48, 0xd6, 0xa8, 0xff];
const PRIMARY_FG: Rgba = [0x06, 0x11, 0x0d, 0xff];
const DANGER: Rgba = [0xf1, 0x5d, 0x6a, 0xff];
const SEC: Rgba = [0x28, 0x31, 0x44, 0xff];

/// フォント（通常・太字）。
#[derive(Debug)]
pub struct Fonts {
    pub regular: FontVec,
    pub bold: FontVec,
}

impl Fonts {
    pub fn load() -> Option<Self> {
        let (regular, bold) = crate::canvas::load_system_fonts()?;
        let bold = match bold {
            Some(b) => b,
            None => crate::canvas::load_system_fonts()?.0,
        };
        Some(Self { regular, bold })
    }
}

/// HUD 通知の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudTone {
    Progress,
    Success,
    Error,
    Info,
}

/// HUD 通知（画面 18）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudMessage {
    pub title: String,
    pub subtitle: String,
    pub tone: HudTone,
}

pub const HUD_W: u32 = 520;
pub const HUD_H: u32 = 124;

/// HUD の画像を描く。
pub fn render_hud(f: &Fonts, m: &HudMessage) -> Canvas {
    let mut c = Canvas::new(HUD_W, HUD_H);
    c.rounded_rect(0.0, 0.0, HUD_W as f32, HUD_H as f32, 24.0, BG);
    c.rounded_rect(1.0, 1.0, HUD_W as f32 - 2.0, HUD_H as f32 - 2.0, 23.0, CARD);
    let (icon_bg, glyph) = match m.tone {
        HudTone::Success => (PRIMARY, "✓"),
        HudTone::Error => (DANGER, "!"),
        HudTone::Progress => (PRIMARY, "↑"),
        HudTone::Info => (SEC, "C"),
    };
    c.circle(62.0, 62.0, 34.0, icon_bg);
    let gw = measure(&f.bold, glyph, 34.0);
    let gfg = if m.tone == HudTone::Info {
        FG
    } else {
        PRIMARY_FG
    };
    c.text(&f.bold, glyph, 62.0 - gw / 2.0, 42.0, 34.0, gfg, None);
    let max = HUD_W as f32 - 130.0;
    c.text(&f.bold, &m.title, 112.0, 26.0, 30.0, FG, Some(max));
    c.text(&f.regular, &m.subtitle, 112.0, 68.0, 22.0, MUTED, Some(max));
    c
}

/// パネルの項目の状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelStatus {
    Ready { remaining: String },
    Processing,
    Failed,
}

/// パネルの 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelItem {
    pub id: String,
    /// 「21:04」
    pub time: String,
    /// 「原寸」「縮小」
    pub quality: String,
    pub is_video: bool,
    pub status: PanelStatus,
    /// サムネイルに使うローカルの画像（パスは表示しない）。
    pub image: Option<PathBuf>,
}

/// パネルの内容。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelModel {
    pub user: String,
    pub auto_upload: bool,
    pub items: Vec<PanelItem>,
    /// 削除の確認を出している項目。
    pub confirm_delete: Option<String>,
}

/// パネルの操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelAction {
    ToggleAutoUpload,
    AskDelete(String),
    ConfirmDelete(String),
    CancelDelete,
    Retry(String),
}

/// 当たり判定の領域。
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: PanelAction,
}

pub const PANEL_W: u32 = 1300;
pub const PANEL_H: u32 = 800;
const CARD_W: f32 = 372.0;
const CARD_H: f32 = 290.0;
const THUMB_W: u32 = 340;
const THUMB_H: u32 = 170;

/// サムネイルの縮小画像（項目 ID ごとに保持）。
#[derive(Debug, Default)]
pub struct ThumbCache {
    items: HashMap<String, Option<Vec<u8>>>,
}

impl ThumbCache {
    fn get(&mut self, id: &str, path: Option<&Path>) -> Option<&Vec<u8>> {
        if !self.items.contains_key(id) {
            let thumb = path.and_then(make_thumb);
            self.items.insert(id.to_owned(), thumb);
        }
        self.items.get(id).and_then(Option::as_ref)
    }

    /// 表示しなくなった項目を捨てる。
    pub fn retain(&mut self, ids: &[&str]) {
        self.items.retain(|k, _| ids.contains(&k.as_str()));
    }
}

/// 中央を切り抜いて 340×170 にする。
fn make_thumb(path: &Path) -> Option<Vec<u8>> {
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let thumb = img.resize_to_fill(THUMB_W, THUMB_H, image::imageops::FilterType::Triangle);
    Some(thumb.to_rgba8().into_raw())
}

// 位置と大きさをそのまま受け取る描画補助。
#[allow(clippy::too_many_arguments)]
fn button(c: &mut Canvas, f: &Fonts, x: f32, y: f32, w: f32, h: f32, label: &str, primary: bool) {
    c.rounded_rect(x, y, w, h, 12.0, if primary { PRIMARY } else { SEC });
    let tw = measure(&f.bold, label, 24.0);
    c.text(
        &f.bold,
        label,
        x + (w - tw) / 2.0,
        y + (h - 30.0) / 2.0,
        24.0,
        if primary { PRIMARY_FG } else { FG },
        None,
    );
}

fn blit(c: &mut Canvas, rgba: &[u8], x0: u32, y0: u32, w: u32, h: u32) {
    for y in 0..h {
        for x in 0..w {
            let si = ((y * w + x) * 4) as usize;
            let (dx, dy) = (x0 + x, y0 + y);
            if dx >= c.width || dy >= c.height {
                continue;
            }
            let di = ((dy * c.width + dx) * 4) as usize;
            if let (Some(s), Some(d)) = (rgba.get(si..si + 4), c.pixels.get_mut(di..di + 4)) {
                d.copy_from_slice(s);
                d[3] = 255;
            }
        }
    }
}

/// パネルを描き、当たり判定を返す。
pub fn render_panel(f: &Fonts, m: &PanelModel, thumbs: &mut ThumbCache) -> (Canvas, Vec<Hit>) {
    let mut c = Canvas::new(PANEL_W, PANEL_H);
    let mut hits = Vec::new();
    c.rounded_rect(
        0.0,
        0.0,
        PANEL_W as f32,
        PANEL_H as f32,
        0.0,
        [0x07, 0x08, 0x0d, 0xff],
    );
    // ヘッダー
    c.rounded_rect(50.0, 36.0, 52.0, 52.0, 14.0, PRIMARY);
    let cw = measure(&f.bold, "C", 28.0);
    c.text(&f.bold, "C", 76.0 - cw / 2.0, 44.0, 28.0, PRIMARY_FG, None);
    c.text(&f.bold, "Clipshare Quick", 120.0, 30.0, 32.0, FG, None);
    c.text(
        &f.regular,
        &format!("直近のアップロード · {}", m.user),
        120.0,
        70.0,
        22.0,
        MUTED,
        Some(700.0),
    );
    // 自動アップロードの切り替え（76×40）
    let (tx, ty) = (PANEL_W as f32 - 50.0 - 76.0, 44.0);
    let label_w = measure(&f.bold, "自動アップロード", 24.0);
    c.text(
        &f.bold,
        "自動アップロード",
        tx - label_w - 70.0,
        48.0,
        24.0,
        FG,
        None,
    );
    c.text(
        &f.bold,
        if m.auto_upload { "ON" } else { "OFF" },
        tx - 56.0,
        48.0,
        24.0,
        if m.auto_upload { PRIMARY } else { MUTED },
        None,
    );
    c.rounded_rect(
        tx,
        ty,
        76.0,
        40.0,
        20.0,
        if m.auto_upload { PRIMARY } else { SEC },
    );
    let knob_x = if m.auto_upload { tx + 56.0 } else { tx + 20.0 };
    c.circle(
        knob_x,
        ty + 20.0,
        15.0,
        if m.auto_upload { PRIMARY_FG } else { MUTED },
    );
    hits.push(Hit {
        x: tx - 20.0,
        y: ty - 10.0,
        w: 116.0,
        h: 60.0,
        action: PanelAction::ToggleAutoUpload,
    });

    for (i, it) in m.items.iter().take(6).enumerate() {
        let col = (i % 3) as f32;
        let row = (i / 3) as f32;
        let x = 50.0 + col * (CARD_W + 42.0);
        let y = 130.0 + row * (CARD_H + 30.0);
        c.rounded_rect(x, y, CARD_W, CARD_H, 18.0, BORDER);
        c.rounded_rect(x + 1.0, y + 1.0, CARD_W - 2.0, CARD_H - 2.0, 17.0, CARD);
        let (tx0, ty0) = (x as u32 + 16, y as u32 + 16);
        match thumbs.get(
            &it.id,
            if it.is_video {
                None
            } else {
                it.image.as_deref()
            },
        ) {
            Some(px) => {
                let px = px.clone();
                blit(&mut c, &px, tx0, ty0, THUMB_W, THUMB_H);
            }
            None => {
                c.rounded_rect(
                    tx0 as f32,
                    ty0 as f32,
                    THUMB_W as f32,
                    THUMB_H as f32,
                    10.0,
                    MUT_BG,
                );
                let label = if it.is_video {
                    "クリップ"
                } else {
                    "写真"
                };
                let lw = measure(&f.bold, label, 26.0);
                c.text(
                    &f.bold,
                    label,
                    tx0 as f32 + (THUMB_W as f32 - lw) / 2.0,
                    ty0 as f32 + 68.0,
                    26.0,
                    MUTED,
                    None,
                );
            }
        }
        let text_y = y + 16.0 + THUMB_H as f32 + 16.0;
        let (line1, line2, line2_color) = match &it.status {
            PanelStatus::Ready { remaining } => (
                format!("{} · {}", it.time, it.quality),
                remaining.clone(),
                MUTED,
            ),
            PanelStatus::Processing => ("処理中".to_owned(), "変換中です".to_owned(), MUTED),
            PanelStatus::Failed => (
                format!("{} · {}", it.time, it.quality),
                "送信に失敗しました".to_owned(),
                DANGER,
            ),
        };
        c.text(
            &f.bold,
            &line1,
            x + 18.0,
            text_y,
            24.0,
            FG,
            Some(CARD_W - 110.0),
        );
        c.text(
            &f.regular,
            &line2,
            x + 18.0,
            text_y + 36.0,
            20.0,
            line2_color,
            Some(CARD_W - 110.0),
        );
        let (bx, by) = (x + CARD_W - 16.0 - 64.0, text_y);
        if it.status == PanelStatus::Failed {
            button(&mut c, f, bx - 36.0, by, 100.0, 64.0, "再試行", true);
            hits.push(Hit {
                x: bx - 36.0,
                y: by,
                w: 100.0,
                h: 64.0,
                action: PanelAction::Retry(it.id.clone()),
            });
        } else {
            button(&mut c, f, bx, by, 64.0, 64.0, "削除", false);
            hits.push(Hit {
                x: bx,
                y: by,
                w: 64.0,
                h: 64.0,
                action: PanelAction::AskDelete(it.id.clone()),
            });
        }
    }

    if let Some(id) = &m.confirm_delete {
        // 確認ダイアログ（ほかの操作は受け付けない）
        hits.clear();
        c.rounded_rect(
            0.0,
            0.0,
            PANEL_W as f32,
            PANEL_H as f32,
            0.0,
            [0, 0, 0, 0x99],
        );
        let (dw, dh) = (620.0, 270.0);
        let (dx, dy) = ((PANEL_W as f32 - dw) / 2.0, (PANEL_H as f32 - dh) / 2.0);
        c.rounded_rect(dx, dy, dw, dh, 22.0, CARD);
        c.text(
            &f.bold,
            "この写真を削除しますか？",
            dx + 36.0,
            dy + 36.0,
            30.0,
            FG,
            None,
        );
        c.text(
            &f.regular,
            "共有URLは使えなくなります。",
            dx + 36.0,
            dy + 86.0,
            22.0,
            MUTED,
            None,
        );
        let (bw, bh) = (200.0, 68.0);
        let by = dy + dh - bh - 32.0;
        button(
            &mut c,
            f,
            dx + dw - 36.0 - bw * 2.0 - 16.0,
            by,
            bw,
            bh,
            "やめる",
            false,
        );
        c.rounded_rect(dx + dw - 36.0 - bw, by, bw, bh, 12.0, DANGER);
        let lw = measure(&f.bold, "削除する", 24.0);
        c.text(
            &f.bold,
            "削除する",
            dx + dw - 36.0 - bw + (bw - lw) / 2.0,
            by + 19.0,
            24.0,
            [0x1a, 0x05, 0x07, 0xff],
            None,
        );
        hits.push(Hit {
            x: dx + dw - 36.0 - bw * 2.0 - 16.0,
            y: by,
            w: bw,
            h: bh,
            action: PanelAction::CancelDelete,
        });
        hits.push(Hit {
            x: dx + dw - 36.0 - bw,
            y: by,
            w: bw,
            h: bh,
            action: PanelAction::ConfirmDelete(id.clone()),
        });
    }
    (c, hits)
}

/// クリック位置（OpenVR のマウス座標は左下が原点）から操作を探す。
pub fn hit_test(hits: &[Hit], x: f32, y_from_bottom: f32) -> Option<PanelAction> {
    let y = PANEL_H as f32 - y_from_bottom;
    hits.iter()
        .find(|h| x >= h.x && x <= h.x + h.w && y >= h.y && y <= h.y + h.h)
        .map(|h| h.action.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> PanelModel {
        PanelModel {
            user: "choko1229".into(),
            auto_upload: true,
            items: vec![
                PanelItem {
                    id: "a".into(),
                    time: "21:04".into(),
                    quality: "原寸".into(),
                    is_video: false,
                    status: PanelStatus::Ready {
                        remaining: "残り 23時間".into(),
                    },
                    image: None,
                },
                PanelItem {
                    id: "b".into(),
                    time: "21:12".into(),
                    quality: "原寸".into(),
                    is_video: false,
                    status: PanelStatus::Failed,
                    image: None,
                },
            ],
            confirm_delete: None,
        }
    }

    #[test]
    fn hit_testing_maps_clicks_to_actions() {
        let Some(f) = Fonts::load() else { return };
        let mut cache = ThumbCache::default();
        let (c, hits) = render_panel(&f, &model(), &mut cache);
        assert_eq!((c.width, c.height), (PANEL_W, PANEL_H));
        let toggle = hits
            .iter()
            .find(|h| h.action == PanelAction::ToggleAutoUpload)
            .unwrap();
        let (cx, cy) = (toggle.x + toggle.w / 2.0, toggle.y + toggle.h / 2.0);
        assert_eq!(
            hit_test(&hits, cx, PANEL_H as f32 - cy),
            Some(PanelAction::ToggleAutoUpload)
        );
        let del = hits
            .iter()
            .find(|h| h.action == PanelAction::AskDelete("a".into()))
            .unwrap();
        assert_eq!(
            hit_test(&hits, del.x + 1.0, PANEL_H as f32 - (del.y + 1.0)),
            Some(PanelAction::AskDelete("a".into()))
        );
        assert!(
            hits.iter()
                .any(|h| h.action == PanelAction::Retry("b".into()))
        );
        assert_eq!(hit_test(&hits, 1.0, 1.0), None);
    }

    #[test]
    fn confirm_dialog_blocks_other_actions() {
        let Some(f) = Fonts::load() else { return };
        let mut m = model();
        m.confirm_delete = Some("a".into());
        let (_, hits) = render_panel(&f, &m, &mut ThumbCache::default());
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .any(|h| h.action == PanelAction::ConfirmDelete("a".into()))
        );
        assert!(hits.iter().any(|h| h.action == PanelAction::CancelDelete));
    }

    #[test]
    fn hud_renders() {
        let Some(f) = Fonts::load() else { return };
        let c = render_hud(
            &f,
            &HudMessage {
                title: "URLをコピーしました".into(),
                subtitle: "24時間有効 · 1件".into(),
                tone: HudTone::Success,
            },
        );
        assert_eq!(c.pixels.len(), (HUD_W * HUD_H * 4) as usize);
    }

    #[test]
    fn thumbnails_are_cropped_to_size() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image::RgbImage::from_pixel(640, 480, image::Rgb([10, 20, 30]))
            .save(&p)
            .unwrap();
        let t = make_thumb(&p).unwrap();
        assert_eq!(t.len(), (THUMB_W * THUMB_H * 4) as usize);
        let mut cache = ThumbCache::default();
        assert!(cache.get("x", Some(&p)).is_some());
        assert!(cache.get("y", None).is_none());
        cache.retain(&["x"]);
        assert_eq!(cache.items.len(), 1);
    }
}
