//! デザインに合わせた部品（トグル、セグメント、カード、ボタン、バッジなど）。

use egui::{Color32, CornerRadius, Margin, Response, RichText, Sense, Stroke, Ui, Vec2};

use super::theme::{Palette, bold, regular};

/// トグルスイッチ（42×24）。変更されたら `changed()`。
pub fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool, label: &str) -> Response {
    let size = Vec2::new(42.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, label));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let bg = if *on { p.pr } else { p.sec };
        ui.painter().rect_filled(rect, CornerRadius::same(12), bg);
        let x = egui::lerp((rect.left() + 12.0)..=(rect.right() - 12.0), t);
        let knob = if *on { p.pfg } else { p.mfg };
        ui.painter()
            .circle_filled(egui::pos2(x, rect.center().y), 9.0, knob);
    }
    resp.on_hover_text(label)
}

/// 2 択のセグメント（原寸 / 縮小 など）。選択が変わったら `true`。
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    p: &Palette,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(p.mut_)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // 右寄せの行の中でも、選択肢は左から順に並べる。
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                for (v, label) in options {
                    let selected = *value == *v;
                    let text = RichText::new(*label).font(bold(13.0)).color(if selected {
                        p.fg
                    } else {
                        p.mfg
                    });
                    let btn = egui::Button::new(text)
                        .fill(if selected {
                            p.card
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6))
                        .min_size(Vec2::new(56.0, 26.0));
                    if ui.add(btn).clicked() && !selected {
                        *value = *v;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// カード（枠線つきの面）。
pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.bd))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// 色つきの面（注意・情報の帯）。
pub fn tinted<R>(ui: &mut Ui, fill: Color32, stroke: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, stroke))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn button(text: &str, fg: Color32, fill: Color32, stroke: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).font(bold(14.0)).color(fg))
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, stroke))
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 40.0))
}

/// 主ボタン（緑）。
pub fn primary(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.add(button(text, p.pfg, p.pr, p.pr))
}

/// 主ボタン（無効化できる）。
pub fn primary_enabled(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    ui.add_enabled(enabled, button(text, p.pfg, p.pr, p.pr))
}

/// 枠線だけのボタン。
pub fn secondary(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.add(button(text, p.fg, Color32::TRANSPARENT, p.bd))
}

/// 危険な操作のボタン（赤）。
pub fn danger(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.add(button(text, p.dfg, p.de, p.de))
}

/// 小さめのボタン（行内の「再試行」「変更」など）。
pub fn small(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text.to_owned()).font(bold(12.0)).color(p.fg))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0_f32, p.bd))
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 28.0)),
    )
}

/// 赤枠の小さいボタン（設定「一般」の「アンインストール」）。
pub fn small_danger(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text.to_owned()).font(bold(12.0)).color(p.det))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0_f32, p.det))
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 28.0)),
    )
}

/// アイコンだけのボタン（URLをコピー・ブラウザで開く・削除）。`label` はツールチップと読み上げ用。
pub fn icon_button(ui: &mut Ui, p: &Palette, glyph: &str, label: &str) -> Response {
    let r = ui.scope(|ui| {
        ui.spacing_mut().button_padding = Vec2::new(4.0, 4.0);
        ui.add(
            egui::Button::new(RichText::new(glyph.to_owned()).size(14.0).color(p.mfg))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .corner_radius(CornerRadius::same(6))
                .min_size(Vec2::new(26.0, 26.0)),
        )
    });
    r.inner.on_hover_text(label)
}

/// 状態のバッジ（処理中・失敗・期限切れ・待機中など）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Danger,
    Warning,
    Muted,
}

pub fn badge(ui: &mut Ui, p: &Palette, text: &str, tone: Tone) {
    let (fg, bg) = match tone {
        Tone::Primary => (p.prt, p.prs),
        Tone::Danger => (p.det, p.des),
        Tone::Warning => (p.wa, p.was),
        Tone::Muted => (p.mfg, p.mut_),
    };
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text.to_owned()).font(bold(11.0)).color(fg));
        });
}

/// ロゴ（緑の角丸に「C」）。
pub fn logo(ui: &mut Ui, p: &Palette, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let r = (size * 0.28) as u8;
    ui.painter().rect_filled(rect, CornerRadius::same(r), p.pr);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "C",
        bold(size * 0.5),
        p.pfg,
    );
}

/// 頭文字のアバター。
pub fn avatar(ui: &mut Ui, p: &Palette, name: &str, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, p.prs);
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".into());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        bold(size * 0.45),
        p.prt,
    );
}

/// 見出し・本文・補足のテキスト。
pub fn title(text: &str, p: &Palette) -> RichText {
    RichText::new(text.to_owned()).font(bold(20.0)).color(p.fg)
}

pub fn heading(text: &str, p: &Palette) -> RichText {
    RichText::new(text.to_owned()).font(bold(15.0)).color(p.fg)
}

pub fn body(text: &str, p: &Palette) -> RichText {
    RichText::new(text.to_owned())
        .font(regular(14.0))
        .color(p.fg)
}

pub fn muted(text: &str, p: &Palette) -> RichText {
    RichText::new(text.to_owned())
        .font(regular(12.0))
        .color(p.mfg)
}

pub fn strong(text: &str, p: &Palette) -> RichText {
    RichText::new(text.to_owned()).font(bold(14.0)).color(p.fg)
}

/// 区切り線。
pub fn divider(ui: &mut Ui, p: &Palette) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, p.bd);
}

/// 初回セットアップの手順表示（1 説明 / 2 フォルダ / 3 連携）。`current` は 1 始まり。
pub fn steps(ui: &mut Ui, p: &Palette, current: usize) {
    ui.horizontal(|ui| {
        for (i, label) in ["説明", "フォルダ", "連携"].iter().enumerate() {
            let n = i + 1;
            let done = n < current;
            let active = n == current;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            let (fill, fg) = if active || done {
                (p.pr, p.pfg)
            } else {
                (p.mut_, p.mfg)
            };
            ui.painter().circle_filled(rect.center(), 11.0, fill);
            let mark = if done {
                "✓".to_owned()
            } else {
                n.to_string()
            };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                mark,
                bold(12.0),
                fg,
            );
            ui.label(RichText::new(*label).font(bold(13.0)).color(if active {
                p.fg
            } else {
                p.mfg
            }));
            if n < 3 {
                let (line, _) = ui.allocate_exact_size(Vec2::new(28.0, 22.0), Sense::hover());
                ui.painter().line_segment(
                    [line.left_center(), line.right_center()],
                    Stroke::new(1.0_f32, p.bd),
                );
            }
        }
    });
}

/// 下部のボタン列（右寄せ・高さ固定）と、その上の区切り線。ボタンは右から順に追加する。
pub fn bottom_bar<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
        let w = ui.available_width();
        let r = ui
            .allocate_ui_with_layout(
                Vec2::new(w, 44.0),
                egui::Layout::right_to_left(egui::Align::Center),
                add,
            )
            .inner;
        ui.add_space(12.0);
        divider(ui, p);
        r
    })
    .inner
}

/// 進捗バー。
pub fn progress_bar(ui: &mut Ui, p: &Palette, fraction: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 6.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), p.mut_);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, CornerRadius::same(3), p.pr);
}

/// 回転するマーク（アップロード中）。
pub fn spinner(ui: &mut Ui, p: &Palette, size: f32) {
    ui.add(egui::Spinner::new().size(size).color(p.pr));
}
