//! オーバーレイ用の簡易ソフトウェア描画（RGBA）。日本語は Windows のシステムフォントで描く。

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};

/// RGBA の色。
pub type Rgba = [u8; 4];

/// 描画先。
#[derive(Debug, Clone)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: Rgba, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let i = ((y as u32 * self.width + x as u32) * 4) as usize;
        let Some(px) = self.pixels.get_mut(i..i + 4) else {
            return;
        };
        let a = coverage.clamp(0.0, 1.0) * f32::from(c[3]) / 255.0;
        for k in 0..3 {
            px[k] = (f32::from(c[k]) * a + f32::from(px[k]) * (1.0 - a)).round() as u8;
        }
        px[3] = ((a + f32::from(px[3]) / 255.0 * (1.0 - a)) * 255.0)
            .round()
            .min(255.0) as u8;
    }

    /// 角丸の四角形。
    pub fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        let r = r.min(w / 2.0).min(h / 2.0);
        let (x0, y0) = (x.floor() as i32, y.floor() as i32);
        let (x1, y1) = ((x + w).ceil() as i32, (y + h).ceil() as i32);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let dx = (x + r - fx).max(fx - (x + w - r)).max(0.0);
                let dy = (y + r - fy).max(fy - (y + h - r)).max(0.0);
                let d = (dx * dx + dy * dy).sqrt() - r;
                // 外周はアンチエイリアス、矩形の外は 0
                let inside_x = (fx - x).min(x + w - fx) + 0.5;
                let inside_y = (fy - y).min(y + h - fy) + 0.5;
                let cover =
                    (0.5 - d).clamp(0.0, 1.0) * inside_x.clamp(0.0, 1.0) * inside_y.clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(px, py, c, cover);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgba) {
        let (x0, y0, x1, y1) = (
            (cx - r - 1.0) as i32,
            (cy - r - 1.0) as i32,
            (cx + r + 1.0) as i32,
            (cy + r + 1.0) as i32,
        );
        for py in y0..=y1 {
            for px in x0..=x1 {
                let (fx, fy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                let cover = (r - (fx * fx + fy * fy).sqrt() + 0.5).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(px, py, c, cover);
                }
            }
        }
    }

    /// 文字列を描く（左上基準）。描いた幅を返す。`max_w` を超える分は「…」で切る。
    // 描画の基本操作なので座標・大きさ・色をそのまま受け取る。
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        font: &FontVec,
        s: &str,
        x: f32,
        y: f32,
        size: f32,
        c: Rgba,
        max_w: Option<f32>,
    ) -> f32 {
        let text = match max_w {
            Some(w) => truncate_to_width(font, s, size, w),
            None => s.to_owned(),
        };
        let scaled = font.as_scaled(PxScale::from(size));
        let mut pen = x;
        let baseline = y + scaled.ascent();
        let mut prev = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(p) = prev {
                pen += scaled.kern(p, id);
            }
            let glyph =
                id.with_scale_and_position(PxScale::from(size), ab_glyph::point(pen, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let b = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    self.blend(
                        b.min.x as i32 + gx as i32,
                        b.min.y as i32 + gy as i32,
                        c,
                        cov,
                    );
                });
            }
            pen += scaled.h_advance(id);
            prev = Some(id);
        }
        pen - x
    }
}

/// 文字列の幅。
pub fn measure(font: &FontVec, s: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut w = 0.0;
    let mut prev = None;
    for ch in s.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            w += scaled.kern(p, id);
        }
        w += scaled.h_advance(id);
        prev = Some(id);
    }
    w
}

fn truncate_to_width(font: &FontVec, s: &str, size: f32, max_w: f32) -> String {
    if measure(font, s, size) <= max_w {
        return s.to_owned();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let candidate = format!("{out}{ch}…");
        if measure(font, &candidate, size) > max_w {
            break;
        }
        out.push(ch);
    }
    format!("{out}…")
}

/// Windows のフォントを読み込む（Yu Gothic UI → Meiryo）。太字があれば 2 つ目に返す。
pub fn load_system_fonts() -> Option<(FontVec, Option<FontVec>)> {
    let dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let load = |name: &str, idx: u32| -> Option<FontVec> {
        let bytes = std::fs::read(dir.join(name)).ok()?;
        FontVec::try_from_vec_and_index(bytes, idx).ok()
    };
    let regular = load("YuGothR.ttc", 1)
        .or_else(|| load("meiryo.ttc", 2))
        .or_else(|| load("msgothic.ttc", 0))?;
    let bold = load("YuGothB.ttc", 1).or_else(|| load("meiryob.ttc", 2));
    Some((regular, bold))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_draw_inside_bounds() {
        let mut c = Canvas::new(40, 20);
        c.rounded_rect(0.0, 0.0, 40.0, 20.0, 6.0, [255, 0, 0, 255]);
        let center = ((10 * 40 + 20) * 4) as usize;
        assert_eq!(&c.pixels[center..center + 4], &[255, 0, 0, 255]);
        // 角は抜ける
        assert!(c.pixels[3] < 128);
        c.circle(100.0, 100.0, 5.0, [0, 0, 0, 255]); // 範囲外でも落ちない
    }

    #[cfg(windows)]
    #[test]
    fn japanese_text_renders_and_truncates() {
        let Some((f, _)) = load_system_fonts() else {
            return;
        };
        let mut c = Canvas::new(200, 40);
        let w = c.text(
            &f,
            "URLをコピーしました",
            4.0,
            4.0,
            18.0,
            [255, 255, 255, 255],
            None,
        );
        assert!(w > 50.0);
        assert!(c.pixels.chunks(4).any(|p| p[3] > 0));
        let t = truncate_to_width(&f, "とても長いファイル名のテストです.png", 16.0, 80.0);
        assert!(t.ends_with('…'));
        assert!(measure(&f, &t, 16.0) <= 80.0 + 0.5);
    }
}
