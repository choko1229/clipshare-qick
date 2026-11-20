//! デザイントークン（`docs/quickshare/design/` の配色）と egui のスタイル設定。

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

/// 配色（デザインの CSS 変数に対応）。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub fg: Color32,
    pub mut_: Color32,
    pub mfg: Color32,
    pub card: Color32,
    pub bd: Color32,
    pub pr: Color32,
    pub pfg: Color32,
    pub prt: Color32,
    pub prs: Color32,
    pub sec: Color32,
    pub de: Color32,
    pub dfg: Color32,
    pub det: Color32,
    pub des: Color32,
    pub wa: Color32,
    pub was: Color32,
    pub dark: bool,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color32 {
    // 不透明度つきの色（前乗算）。
    let a8 = (a * 255.0) as u8;
    Color32::from_rgba_premultiplied(
        ((r as u32 * a8 as u32) / 255) as u8,
        ((g as u32 * a8 as u32) / 255) as u8,
        ((b as u32 * a8 as u32) / 255) as u8,
        a8,
    )
}

pub const DARK: Palette = Palette {
    bg: hex(0x07080d),
    fg: hex(0xf7f7fb),
    mut_: hex(0x151821),
    mfg: hex(0xa3a8b8),
    card: hex(0x10131b),
    bd: hex(0x252a38),
    pr: hex(0x48d6a8),
    pfg: hex(0x06110d),
    prt: hex(0x48d6a8),
    prs: rgba(72, 214, 168, 0.14),
    sec: hex(0x283144),
    de: hex(0xf15d6a),
    dfg: hex(0x1a0507),
    det: hex(0xf15d6a),
    des: rgba(241, 93, 106, 0.14),
    wa: hex(0xf5b84a),
    was: rgba(245, 184, 74, 0.14),
    dark: true,
};

pub const LIGHT: Palette = Palette {
    bg: hex(0xf7f8fb),
    fg: hex(0x12141c),
    mut_: hex(0xe8ebf2),
    mfg: hex(0x5a6272),
    card: hex(0xffffff),
    bd: hex(0xd9deea),
    pr: hex(0x0f9f78),
    pfg: hex(0x06110d),
    prt: hex(0x0a7a5c),
    prs: rgba(15, 159, 120, 0.12),
    sec: hex(0xe2e7f0),
    de: hex(0xc4283a),
    dfg: hex(0xffffff),
    det: hex(0xb8212f),
    des: rgba(196, 40, 58, 0.10),
    wa: hex(0x8a5300),
    was: rgba(180, 110, 0, 0.12),
    dark: false,
};

/// Windows の「アプリ モード」（ライト/ダーク）に合わせる。
pub fn system_palette() -> Palette {
    if crate::platform::apps_use_light_theme() {
        LIGHT
    } else {
        DARK
    }
}

/// 見出し用の太字フォント。
pub fn bold_family() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, bold_family())
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// 日本語フォントを Windows のシステムフォントから読み込む（同梱しない）。
fn load_fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let pick = |names: &[(&str, u32)]| -> Option<FontData> {
        names.iter().find_map(|(n, idx)| {
            std::fs::read(dir.join(n)).ok().map(|bytes| {
                let mut fd = FontData::from_owned(bytes);
                fd.index = *idx;
                fd
            })
        })
    };
    // Yu Gothic UI（游ゴシック UI）を優先。ttc の索引は 1 が UI 版。
    let regular = pick(&[
        ("YuGothR.ttc", 1),
        ("YuGothM.ttc", 0),
        ("meiryo.ttc", 2),
        ("msgothic.ttc", 0),
    ]);
    let bold = pick(&[("YuGothB.ttc", 1), ("meiryob.ttc", 2)]);
    if let Some(r) = regular {
        fonts.font_data.insert("jp".into(), Arc::new(r));
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(fam)
                .or_default()
                .insert(0, "jp".into());
        }
    } else {
        tracing::warn!("日本語フォントが見つかりません");
    }
    let mut bold_list = Vec::new();
    if let Some(b) = bold {
        fonts.font_data.insert("jp-bold".into(), Arc::new(b));
        bold_list.push("jp-bold".to_owned());
    }
    bold_list.push("jp".to_owned());
    bold_list.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    bold_list.dedup();
    fonts.families.insert(bold_family(), bold_list);
    fonts
}

/// egui にフォントと配色を設定する。
pub fn apply(ctx: &egui::Context, p: &Palette) {
    ctx.set_fonts(load_fonts());
    let mut visuals = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = p.bg;
    visuals.window_fill = p.card;
    visuals.extreme_bg_color = p.mut_;
    visuals.faint_bg_color = p.card;
    visuals.override_text_color = Some(p.fg);
    visuals.hyperlink_color = p.prt;
    visuals.selection.bg_fill = p.prs;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, p.pr);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, p.bd);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, p.fg);
    visuals.widgets.inactive.bg_fill = p.mut_;
    visuals.widgets.inactive.weak_bg_fill = p.mut_;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, p.bd);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, p.fg);
    visuals.widgets.hovered.bg_fill = p.sec;
    visuals.widgets.hovered.weak_bg_fill = p.sec;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, p.bd);
    visuals.widgets.active.bg_fill = p.sec;
    visuals.widgets.active.weak_bg_fill = p.sec;
    for w in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.noninteractive,
    ] {
        w.corner_radius = egui::CornerRadius::same(8);
    }
    ctx.set_visuals(visuals);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(16.0, 8.0);
        s.spacing.interact_size.y = 32.0;
        s.text_styles.insert(TextStyle::Body, regular(14.0));
        s.text_styles.insert(TextStyle::Button, bold(14.0));
        s.text_styles.insert(TextStyle::Small, regular(12.0));
        s.text_styles.insert(TextStyle::Heading, bold(20.0));
        s.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        );
    });
}
