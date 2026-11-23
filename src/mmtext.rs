//! Burmese (Myanmar script) text. egui cannot shape Myanmar (vowel reordering, stacked
//! consonants, mark positioning), so any line that contains it is shaped here with
//! rustybuzz and drawn into an RGBA bitmap that the UI shows as an image.
//! Latin text inside such a line uses DejaVu Sans Mono (Noto Sans Myanmar has no Latin).

use ab_glyph::{point, Font, FontRef, GlyphId};

static MM_FONT: &[u8] = include_bytes!("../assets/NotoSansMyanmar-Regular.ttf");
static LATIN_FONT: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");

pub fn is_mm(c: char) -> bool {
    matches!(c, '\u{1000}'..='\u{109F}' | '\u{A9E0}'..='\u{A9FF}' | '\u{AA60}'..='\u{AA7F}')
}

pub fn contains_myanmar(s: &str) -> bool {
    s.chars().any(is_mm)
}

struct Font2 {
    shape: rustybuzz::Face<'static>,
    ab: FontRef<'static>,
}

impl Font2 {
    fn new(data: &'static [u8]) -> Option<Self> {
        Some(Self {
            shape: rustybuzz::Face::from_slice(data, 0)?,
            ab: FontRef::try_from_slice(data).ok()?,
        })
    }
}

struct Glyph {
    id: u16,
    x_off: f32,
    y_off: f32,
    advance: f32,
}

pub struct Shaper {
    mm: Font2,
    latin: Font2,
}

impl Shaper {
    pub fn new() -> Option<Self> {
        Some(Self {
            mm: Font2::new(MM_FONT)?,
            latin: Font2::new(LATIN_FONT)?,
        })
    }

    /// Split into runs: Myanmar script (true) vs everything else (false).
    fn runs(text: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut kind: Option<bool> = None;
        for (i, c) in text.char_indices() {
            let k = is_mm(c);
            if kind.is_some() && kind != Some(k) {
                out.push((kind.unwrap(), &text[start..i]));
                start = i;
            }
            kind = Some(k);
        }
        if let Some(k) = kind {
            out.push((k, &text[start..]));
        }
        out
    }

    fn shape_run(font: &Font2, text: &str, px: f32) -> Vec<Glyph> {
        let scale = px / font.shape.units_per_em() as f32;
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(text);
        buf.guess_segment_properties();
        let out = rustybuzz::shape(&font.shape, &[], buf);
        out.glyph_infos()
            .iter()
            .zip(out.glyph_positions())
            .map(|(i, p)| Glyph {
                id: i.glyph_id as u16,
                x_off: p.x_offset as f32 * scale,
                y_off: p.y_offset as f32 * scale,
                advance: p.x_advance as f32 * scale,
            })
            .collect()
    }

    fn shaped(&self, text: &str, px: f32) -> Vec<(bool, Vec<Glyph>)> {
        Self::runs(text)
            .into_iter()
            .map(|(is_mm, s)| {
                let font = if is_mm { &self.mm } else { &self.latin };
                (is_mm, Self::shape_run(font, s, px))
            })
            .collect()
    }

    pub fn width(&self, text: &str, px: f32) -> f32 {
        self.shaped(text, px)
            .iter()
            .flat_map(|(_, g)| g.iter())
            .map(|g| g.advance)
            .sum()
    }

    /// Greedy line wrapping at spaces and Burmese syllable boundaries.
    pub fn wrap(&self, text: &str, px: f32, max_w: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut cur = String::new();
        for u in units(text) {
            let trial = format!("{cur}{u}");
            if !cur.is_empty() && self.width(trial.trim_end(), px) > max_w {
                lines.push(cur.trim_end().to_string());
                cur = u;
            } else {
                cur = trial;
            }
        }
        if !cur.trim().is_empty() {
            lines.push(cur.trim_end().to_string());
        }
        lines
    }

    /// Draw one line. Returns (width, height, RGBA bytes) - colour `rgb`, coverage in alpha.
    pub fn render(&self, text: &str, px: f32, rgb: [u8; 3]) -> Option<(usize, usize, Vec<u8>)> {
        let runs = self.shaped(text, px);
        let total: f32 = runs.iter().flat_map(|(_, g)| g.iter()).map(|g| g.advance).sum();
        let pad = 4.0;
        let asc = px * 1.1;
        let desc = px * 0.6;
        let w = (total + pad * 2.0).ceil() as usize;
        let h = (asc + desc + pad * 2.0).ceil() as usize;
        if w == 0 || h == 0 || w > 8192 {
            return None;
        }
        let mut pix = vec![0u8; w * h * 4];
        for px_i in pix.chunks_exact_mut(4) {
            px_i[0] = rgb[0];
            px_i[1] = rgb[1];
            px_i[2] = rgb[2];
        }
        let baseline = pad + asc;
        let mut pen = pad;
        for (is_mm, glyphs) in &runs {
            let font = if *is_mm { &self.mm } else { &self.latin };
            // PxScale is the ascent+descent height in px (pt_to_px_scale assumes 96 dpi, so avoid it)
            let upem = font.ab.units_per_em()?;
            let scale = ab_glyph::PxScale::from(px * font.ab.height_unscaled() / upem);
            for g in glyphs {
                let glyph = GlyphId(g.id).with_scale_and_position(
                    scale,
                    point(pen + g.x_off, baseline - g.y_off),
                );
                if let Some(og) = font.ab.outline_glyph(glyph) {
                    let b = og.px_bounds();
                    let (bx, by) = (b.min.x.floor() as i32, b.min.y.floor() as i32);
                    og.draw(|x, y, c| {
                        let ix = bx + x as i32;
                        let iy = by + y as i32;
                        if ix >= 0 && iy >= 0 && (ix as usize) < w && (iy as usize) < h {
                            let idx = (iy as usize * w + ix as usize) * 4 + 3;
                            let a = (c.clamp(0.0, 1.0) * 255.0) as u8;
                            if a > pix[idx] {
                                pix[idx] = a;
                            }
                        }
                    });
                }
                pen += g.advance;
            }
        }
        Some((w, h, pix))
    }
}

fn final_consonant(ch: &[char], i: usize) -> bool {
    let mut j = i + 1;
    while j < ch.len() && ('\u{102B}'..='\u{1038}').contains(&ch[j]) {
        j += 1;
    }
    j < ch.len() && (ch[j] == '\u{103A}' || ch[j] == '\u{1039}')
}

/// Pieces a line may be broken between: words (after a space) and Burmese syllables.
fn units(text: &str) -> Vec<String> {
    let ch: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for i in 0..ch.len() {
        let c = ch[i];
        if c == ' ' {
            cur.push(c);
            out.push(std::mem::take(&mut cur));
            continue;
        }
        let starts = ('\u{1000}'..='\u{1021}').contains(&c)
            && cur.chars().last().map_or(false, is_mm)
            && !(i > 0 && ch[i - 1] == '\u{1039}')
            && !final_consonant(&ch, i);
        if starts {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_burmese_line() {
        let s = Shaper::new().expect("fonts");
        let text = "version: v1.2.1 မြန်မာဘာသာစကားဖြင့် Programming ရေးသားနိုင်ပြီ";
        let lines = s.wrap(text, 48.0, 500.0);
        assert!(lines.len() >= 2, "should wrap, got {lines:?}");
        let (w, h, pix) = s.render(&lines[0], 48.0, [0, 0, 0]).expect("render");
        assert!(pix.chunks_exact(4).any(|p| p[3] > 128));
        let img = image::RgbaImage::from_raw(w as u32, h as u32, pix).unwrap();
        let mut bg = image::RgbaImage::from_pixel(w as u32, h as u32, image::Rgba([184, 229, 255, 255]));
        image::imageops::overlay(&mut bg, &img, 0, 0);
        bg.save("/tmp/mm_test.png").unwrap();
    }
}
