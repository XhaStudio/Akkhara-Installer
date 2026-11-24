//! Release-notes markdown: a pure parser (no UI) plus an egui renderer.
//!
//! Supported:
//!   # H1 ... ###### H6          headings
//!   **bold** __bold__            *italic* _italic_     ***bold italic***
//!   ~~strikethrough~~            `inline code`         \* escapes
//!   [text](url)  <https://..>  bare https://.. URLs    ![alt](img) (shown as [alt])
//!   - * + bullets (nested), 1. numbered, - [ ] / - [x] task lists
//!   > quotes (nested)           ---  rules             ``` fenced code ```
//!   | tables |                  <br> and common HTML tags are handled
//! Lines containing Burmese are shaped and drawn as images (see mmtext.rs).

use std::{collections::HashMap, rc::Rc};

// ============================================================== parser

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Space,
    Rule,
    Heading(u8, Vec<Span>),
    Para(Vec<Span>),
    Bullet { level: usize, check: Option<bool>, spans: Vec<Span> },
    Numbered { level: usize, num: String, spans: Vec<Span> },
    Quote { depth: usize, spans: Vec<Span> },
    Code(Vec<String>),
    Table { header: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>> },
}

pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn run_len(c: &[char], i: usize, ch: char) -> usize {
    let mut n = 0;
    while i + n < c.len() && c[i + n] == ch {
        n += 1;
    }
    n
}

/// Next run of exactly `n` copies of `ch` at or after `from`.
fn find_run(c: &[char], from: usize, ch: char, n: usize) -> Option<usize> {
    let mut j = from;
    while j < c.len() {
        if c[j] == ch {
            let m = run_len(c, j, ch);
            if m == n {
                return Some(j);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

fn starts_with_at(c: &[char], i: usize, pat: &str) -> bool {
    pat.chars().enumerate().all(|(k, p)| c.get(i + k) == Some(&p))
}

/// Is there a valid closing delimiter run after `from`?
fn has_closer(c: &[char], from: usize, ch: char, used: usize) -> bool {
    let mut j = from;
    while j < c.len() {
        match c[j] {
            '\\' => j += 2,
            '`' => {
                let n = run_len(c, j, '`');
                match find_run(c, j + n, '`', n) {
                    Some(e) => j = e + n,
                    None => j += n,
                }
            }
            x if x == ch => {
                let m = run_len(c, j, ch);
                let after_alnum = j + m < c.len() && c[j + m].is_alphanumeric();
                if m >= used && j > from && !c[j - 1].is_whitespace() && (ch != '_' || !after_alnum) {
                    return true;
                }
                j += m;
            }
            _ => j += 1,
        }
    }
    false
}

/// `[label](url)` starting at `c[i] == '['`. Returns (label, url, index after `)`).
fn parse_link(c: &[char], i: usize) -> Option<(String, String, usize)> {
    let mut depth = 0i32;
    let mut j = i;
    let mut close = None;
    while j < c.len() {
        match c[j] {
            '\\' => {
                j += 2;
                continue;
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    let close = close?;
    if c.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut k = close + 2;
    let mut pd = 1;
    while k < c.len() {
        match c[k] {
            '(' => pd += 1,
            ')' => {
                pd -= 1;
                if pd == 0 {
                    break;
                }
            }
            _ => {}
        }
        k += 1;
    }
    if k >= c.len() {
        return None;
    }
    let label: String = c[i + 1..close].iter().collect();
    let url: String = c[close + 2..k].iter().collect();
    Some((label, url.trim().to_string(), k + 1))
}

const HTML_TAGS: [&str; 33] = [
    "b", "i", "u", "em", "strong", "p", "div", "span", "br", "hr", "img", "a", "details", "summary",
    "sub", "sup", "pre", "center", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "table",
    "thead", "tbody", "tr", "td", "th",
];

/// If `c[i]` starts a known HTML tag, returns the index after its `>`.
fn html_tag_end(c: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if c.get(j) == Some(&'/') {
        j += 1;
    }
    let start = j;
    while j < c.len() && c[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == start {
        return None;
    }
    let name: String = c[start..j].iter().collect::<String>().to_lowercase();
    if !HTML_TAGS.contains(&name.as_str()) {
        return None;
    }
    let limit = (j + 300).min(c.len());
    (j..limit).find(|&k| c[k] == '>').map(|k| k + 1)
}

fn sp(text: &str, f: impl FnOnce(&mut Span)) -> Span {
    let mut s = Span {
        text: text.to_string(),
        ..Default::default()
    };
    f(&mut s);
    s
}

/// Inline markdown -> styled spans.
pub fn inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic, mut strike) = (false, false, false);
    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span {
                    text: std::mem::take(&mut buf),
                    bold,
                    italic,
                    strike,
                    ..Default::default()
                });
            }
        };
    }
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        // backslash escape
        if c == '\\' {
            if let Some(&n) = chars.get(i + 1) {
                if n.is_ascii_punctuation() {
                    buf.push(n);
                    i += 2;
                    continue;
                }
            }
            buf.push(c);
            i += 1;
            continue;
        }

        // `code`
        if c == '`' {
            let n = run_len(&chars, i, '`');
            if let Some(end) = find_run(&chars, i + n, '`', n) {
                flush!();
                let mut content: String = chars[i + n..end].iter().collect();
                if content.len() > 1 && content.starts_with(' ') && content.ends_with(' ') && !content.trim().is_empty() {
                    content = content[1..content.len() - 1].to_string();
                }
                spans.push(Span {
                    text: content,
                    bold,
                    italic,
                    strike,
                    code: true,
                    ..Default::default()
                });
                i = end + n;
            } else {
                for _ in 0..n {
                    buf.push('`');
                }
                i += n;
            }
            continue;
        }

        // ![alt](url)
        if c == '!' && chars.get(i + 1) == Some(&'[') {
            if let Some((alt, _url, next)) = parse_link(&chars, i + 1) {
                flush!();
                spans.push(sp(&format!("[{}]", alt), |s| s.italic = true));
                i = next;
                continue;
            }
        }

        // [text](url)
        if c == '[' {
            if let Some((label, url, next)) = parse_link(&chars, i) {
                flush!();
                let inner = if label.trim().is_empty() { url } else { label };
                for mut s in inline(&inner) {
                    s.link = true;
                    spans.push(s);
                }
                i = next;
                continue;
            }
        }

        // <https://autolink> and known HTML tags
        if c == '<' {
            if starts_with_at(&chars, i, "<http") {
                if let Some(k) = (i + 1..chars.len()).find(|&k| chars[k] == '>' || chars[k].is_whitespace()) {
                    if chars[k] == '>' {
                        flush!();
                        let url: String = chars[i + 1..k].iter().collect();
                        spans.push(sp(&url, |s| s.link = true));
                        i = k + 1;
                        continue;
                    }
                }
            }
            if let Some(end) = html_tag_end(&chars, i) {
                flush!();
                i = end;
                continue;
            }
        }

        // bare URL
        if c == 'h'
            && (i == 0 || !chars[i - 1].is_alphanumeric())
            && (starts_with_at(&chars, i, "http://") || starts_with_at(&chars, i, "https://"))
        {
            let mut e = i;
            while e < chars.len() && !chars[e].is_whitespace() && chars[e] != '<' && chars[e] != '>' {
                e += 1;
            }
            while e > i && ".,;:!?)]}'\"".contains(chars[e - 1]) {
                e -= 1;
            }
            flush!();
            let url: String = chars[i..e].iter().collect();
            spans.push(sp(&url, |s| s.link = true));
            i = e;
            continue;
        }

        // emphasis: * _ ~
        if c == '*' || c == '_' || c == '~' {
            let n = run_len(&chars, i, c);
            let used = if c == '~' { if n >= 2 { 2 } else { 0 } } else { n.min(3) };
            if used == 0 {
                buf.push(c);
                i += 1;
                continue;
            }
            let (tb, ti, ts) = match (c, used) {
                ('~', _) => (false, false, true),
                (_, 1) => (false, true, false),
                (_, 2) => (true, false, false),
                _ => (true, true, false),
            };
            let is_open = !((tb && bold) || (ti && italic) || (ts && strike));
            let before_ws = i == 0 || chars[i - 1].is_whitespace();
            let after_ws = i + used >= chars.len() || chars[i + used].is_whitespace();
            let prev_alnum = i > 0 && chars[i - 1].is_alphanumeric();
            let next_alnum = i + used < chars.len() && chars[i + used].is_alphanumeric();
            let valid = if is_open {
                !after_ws && (c != '_' || !prev_alnum) && has_closer(&chars, i + used, c, used)
            } else {
                !before_ws && (c != '_' || !next_alnum)
            };
            if valid {
                flush!();
                if tb {
                    bold = !bold;
                }
                if ti {
                    italic = !italic;
                }
                if ts {
                    strike = !strike;
                }
            } else {
                for _ in 0..used {
                    buf.push(c);
                }
            }
            i += used;
            continue;
        }

        buf.push(c);
        i += 1;
    }
    flush!();
    spans
}

fn is_hr(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3
        && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

fn heading_of(t: &str) -> Option<(u8, &str)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&n) {
        return None;
    }
    let rest = &t[n..];
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let h = rest.trim();
    let stripped = h.trim_end_matches('#');
    let h = if stripped.is_empty() || stripped.ends_with(' ') { stripped.trim_end() } else { h };
    Some((n as u8, h))
}

fn numbered_of(t: &str) -> Option<(&str, &str)> {
    let d = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&d) {
        let rest = &t[d..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some((&t[..d], rest[2..].trim_start()));
        }
    }
    None
}

fn is_table_sep(s: &str) -> bool {
    s.contains('-') && s.contains('|') && s.chars().all(|c| "|-: \t".contains(c))
}

fn split_row(line: &str) -> Vec<Vec<Span>> {
    let s = line.trim();
    let s = s.strip_prefix('|').unwrap_or(s);
    let s = s.strip_suffix('|').unwrap_or(s);
    s.split('|').map(|c| inline(c.trim())).collect()
}

fn fence_open(t: &str) -> Option<(char, usize)> {
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let n = t.chars().take_while(|c| *c == ch).count();
    if n >= 3 { Some((ch, n)) } else { None }
}

fn is_fence_close(t: &str, ch: char, n: usize) -> bool {
    t.len() >= n && t.chars().all(|c| c == ch)
}

fn push_block(out: &mut Vec<Block>, b: Block) {
    if matches!(b, Block::Space) && matches!(out.last(), Some(Block::Space) | None) {
        return; // no leading / doubled blank lines
    }
    out.push(b);
}

pub fn parse(text: &str) -> Vec<Block> {
    let text = text
        .replace("\r\n", "\n")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Block> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut code: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i].trim_end();
        let t = raw.trim_start();

        if let Some((fc, fl)) = fence {
            if is_fence_close(t, fc, fl) {
                out.push(Block::Code(std::mem::take(&mut code)));
                fence = None;
            } else {
                code.push(raw.to_string());
            }
            i += 1;
            continue;
        }
        if let Some(f) = fence_open(t) {
            fence = Some(f);
            i += 1;
            continue;
        }
        if t.is_empty() {
            push_block(&mut out, Block::Space);
            i += 1;
            continue;
        }
        if is_hr(t) {
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if let Some((n, h)) = heading_of(t) {
            out.push(Block::Heading(n, inline(h)));
            i += 1;
            continue;
        }
        // table: a row line followed by a |---|---| separator
        if t.contains('|') && i + 1 < lines.len() && is_table_sep(lines[i + 1].trim()) {
            let header = split_row(t);
            let mut rows = Vec::new();
            i += 2;
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                rows.push(split_row(lines[i]));
                i += 1;
            }
            out.push(Block::Table { header, rows });
            continue;
        }
        // block quote (nested: > > text)
        if t.starts_with('>') {
            let mut depth = 0;
            let mut rest = t;
            loop {
                let r = rest.trim_start();
                if let Some(x) = r.strip_prefix('>') {
                    depth += 1;
                    rest = x;
                } else {
                    rest = r;
                    break;
                }
            }
            out.push(Block::Quote { depth, spans: inline(rest) });
            i += 1;
            continue;
        }
        // lists
        let lead: usize = raw
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let level = (lead / 2).min(5);
        let bullet = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "));
        if let Some(rest) = bullet {
            let (check, rest) = if let Some(r) = rest.strip_prefix("[ ] ") {
                (Some(false), r)
            } else if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
                (Some(true), r)
            } else {
                (None, rest)
            };
            out.push(Block::Bullet { level, check, spans: inline(rest.trim_start()) });
            i += 1;
            continue;
        }
        if let Some((n, rest)) = numbered_of(t) {
            out.push(Block::Numbered { level, num: n.to_string(), spans: inline(rest) });
            i += 1;
            continue;
        }
        out.push(Block::Para(inline(t)));
        i += 1;
    }
    if fence.is_some() {
        out.push(Block::Code(code)); // unterminated fence
    }
    while matches!(out.last(), Some(Block::Space)) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: &str) -> Vec<Span> {
        inline(t)
    }

    #[test]
    fn headings_h1_to_h6() {
        for n in 1..=6u8 {
            let md = format!("{} Heading {}", "#".repeat(n as usize), n);
            match &parse(&md)[0] {
                Block::Heading(l, s) => {
                    assert_eq!(*l, n);
                    assert_eq!(plain(s), format!("Heading {n}"));
                }
                b => panic!("not a heading: {b:?}"),
            }
        }
        assert!(matches!(parse("####### seven")[0], Block::Para(_)));
        assert!(matches!(parse("#nospace")[0], Block::Para(_)));
    }

    #[test]
    fn emphasis_styles() {
        let s = p("**bold** and __bold2__");
        assert!(s[0].bold && s[0].text == "bold");
        assert!(s[2].bold && s[2].text == "bold2");
        let s = p("*italic* and _italic2_");
        assert!(s[0].italic && s[2].italic);
        let s = p("***both***");
        assert!(s[0].bold && s[0].italic);
        let s = p("~~gone~~ ok");
        assert!(s[0].strike && !s[1].strike);
        let s = p("use `code here` now");
        assert!(s[1].code && s[1].text == "code here");
    }

    #[test]
    fn emphasis_is_not_greedy() {
        // math, snake_case and unmatched markers stay literal
        assert_eq!(plain(&p("2 * 3 * 4")), "2 * 3 * 4");
        assert!(p("snake_case_name").iter().all(|s| !s.italic));
        assert_eq!(plain(&p("a ** b")), "a ** b");
        assert_eq!(plain(&p("lonely *star")), "lonely *star");
        assert_eq!(plain(&p(r"\*not italic\*")), "*not italic*");
    }

    #[test]
    fn code_span_keeps_markers() {
        let s = p("`**x**`");
        assert_eq!(s.len(), 1);
        assert!(s[0].code && s[0].text == "**x**" && !s[0].bold);
    }

    #[test]
    fn links_and_urls() {
        let s = p("see [docs](https://x.io/a) now");
        assert_eq!(plain(&s), "see docs now");
        assert!(s[1].link);
        let s = p("go https://example.com/a.b, ok");
        assert!(s[1].link && s[1].text == "https://example.com/a.b");
        let s = p("<https://e.com>");
        assert!(s[0].link);
        assert_eq!(plain(&p("![logo](a.png)")), "[logo]");
    }

    #[test]
    fn nested_emphasis_in_link_and_bold() {
        let s = p("**bold *and italic* text**");
        assert!(s.iter().any(|x| x.bold && x.italic && x.text == "and italic"));
    }

    #[test]
    fn lists_quotes_rules_code_tables() {
        let md = "- a\n  - b\n1. one\n- [x] done\n- [ ] todo\n> q\n> > deep\n---\n```\ncode\n```\n| h1 | h2 |\n|---|---|\n| x | y |";
        let b = parse(md);
        assert!(matches!(b[0], Block::Bullet { level: 0, check: None, .. }));
        assert!(matches!(b[1], Block::Bullet { level: 1, .. }));
        assert!(matches!(&b[2], Block::Numbered { num, .. } if num == "1"));
        assert!(matches!(b[3], Block::Bullet { check: Some(true), .. }));
        assert!(matches!(b[4], Block::Bullet { check: Some(false), .. }));
        assert!(matches!(b[5], Block::Quote { depth: 1, .. }));
        assert!(matches!(b[6], Block::Quote { depth: 2, .. }));
        assert!(matches!(b[7], Block::Rule));
        assert!(matches!(&b[8], Block::Code(l) if l == &vec!["code".to_string()]));
        assert!(matches!(&b[9], Block::Table { rows, .. } if rows.len() == 1));
    }

    #[test]
    fn burmese_and_html() {
        let s = p("မြန်မာစာ **ကောင်းသည်** ဖြစ်သည်");
        assert!(s.iter().any(|x| x.bold && x.text == "ကောင်းသည်"));
        assert_eq!(plain(&p("a<b>x</b>b")), "axb");
        assert_eq!(plain(&p("akk <file_name>")), "akk <file_name>");
    }

    #[test]
    fn blank_lines_collapse() {
        let b = parse("\n\na\n\n\n\nb\n\n");
        assert_eq!(b.len(), 3);
        assert!(matches!(b[1], Block::Space));
    }
}

// ============================================================ rendering

use crate::mmtext::{contains_myanmar, Shaper};
use crate::{Tex, BLUE, GREY, INK};
use eframe::egui::{self, pos2, vec2, Color32, FontId, Rect, Sense, Stroke};

const CODE_BG: Color32 = Color32::from_rgb(160, 215, 245);
const BOLD_INK: Color32 = Color32::from_rgb(8, 36, 100);
const RULE: Color32 = Color32::from_rgb(90, 120, 150);

pub struct MdCtx<'a> {
    pub shaper: Option<&'a Shaper>,
    pub cache: &'a mut HashMap<String, Tex>,
    pub wraps: &'a mut HashMap<String, Rc<Vec<String>>>,
    pub parsed: &'a mut HashMap<u64, Rc<Vec<Block>>>,
    pub ctx: &'a egui::Context,
}

fn job_of(spans: &[Span], size: f32, force_bold: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for s in spans {
        if s.text.is_empty() {
            continue;
        }
        let bold = s.bold || force_bold;
        let color = if s.link {
            BLUE
        } else if bold {
            BOLD_INK
        } else {
            INK
        };
        let fmt = egui::TextFormat {
            font_id: FontId::monospace(size),
            color,
            italics: s.italic,
            background: if s.code { CODE_BG } else { Color32::TRANSPARENT },
            strikethrough: if s.strike { Stroke::new(1.5_f32, INK) } else { Stroke::NONE },
            underline: if s.link { Stroke::new(1.0_f32, BLUE) } else { Stroke::NONE },
            ..Default::default()
        };
        job.append(&s.text, 0.0, fmt);
    }
    job
}

/// A Burmese line as a texture (cached by text + size).
fn mm_texture(md: &mut MdCtx, line: &str, px: f32) -> Option<(egui::TextureId, egui::Vec2)> {
    let sh = md.shaper?;
    let key = format!("{px}|{line}");
    if !md.cache.contains_key(&key) {
        let (w, h, pix) = sh.render(line, px * 2.0, [0, 0, 0])?;
        let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], &pix);
        let name = format!("md{}", md.cache.len());
        let handle = md.ctx.load_texture(name, ci, egui::TextureOptions::LINEAR);
        md.cache.insert(
            key.clone(),
            Tex {
                handle,
                size: vec2(w as f32 / 2.0, h as f32 / 2.0),
            },
        );
    }
    let t = md.cache.get(&key)?;
    Some((t.handle.id(), t.size))
}

/// One paragraph / heading / list item. Burmese is shaped and drawn as images,
/// everything else is normal styled text.
fn md_block(ui: &mut egui::Ui, md: &mut MdCtx, spans: &[Span], size: f32, bold: bool, indent: f32) {
    let text = plain(spans);
    if contains_myanmar(&text) && md.shaper.is_some() {
        let px = size + 1.5;
        let avail = (ui.available_width() - indent).max(80.0);
        // wrapping needs shaping, which is slow: do it once per (text, size, width), not per frame
        let wkey = format!("{px}|{:.0}|{text}", avail);
        if !md.wraps.contains_key(&wkey) {
            let wrapped = md
                .shaper
                .map(|s| s.wrap(&text, px * 2.0, avail * 2.0))
                .unwrap_or_default();
            md.wraps.insert(wkey.clone(), Rc::new(wrapped));
        }
        let lines = md.wraps.get(&wkey).cloned().unwrap_or_default();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(indent);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for l in lines.iter() {
                    if let Some((id, sz)) = mm_texture(md, l, px) {
                        ui.image((id, sz));
                    }
                }
            });
        });
    } else {
        let job = job_of(spans, size, bold);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(indent);
            ui.vertical(|ui| {
                ui.label(job);
            });
        });
    }
}

fn with_prefix(prefix: &str, spans: &[Span]) -> Vec<Span> {
    let mut v = vec![sp(prefix, |_| {})];
    v.extend_from_slice(spans);
    v
}

fn hline(ui: &mut egui::Ui, height: f32, thickness: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(thickness, RULE));
}

pub fn render(ui: &mut egui::Ui, blocks: &[Block], md: &mut MdCtx) {
    if blocks.is_empty() {
        ui.label(egui::RichText::new("No release notes.").monospace().size(14.0).color(INK));
        return;
    }
    ui.spacing_mut().item_spacing.y = 3.0;
    const HEADING_SIZE: [f32; 6] = [22.0, 19.0, 17.0, 15.0, 14.0, 13.0];
    for (bi, b) in blocks.iter().enumerate() {
        match b {
            Block::Space => ui.add_space(5.0),
            Block::Rule => hline(ui, 12.0, 2.0),
            Block::Heading(n, spans) => {
                let n = (*n).clamp(1, 6) as usize;
                if n <= 2 {
                    ui.add_space(4.0);
                }
                md_block(ui, md, spans, HEADING_SIZE[n - 1], true, 0.0);
                if n <= 2 {
                    hline(ui, 6.0, 1.0);
                }
            }
            Block::Para(spans) => md_block(ui, md, spans, 13.0, false, 0.0),
            Block::Bullet { level, check, spans } => {
                let mark = match check {
                    Some(true) => "[x] ",
                    Some(false) => "[ ] ",
                    None if *level == 0 => "• ",
                    None => "– ",
                };
                md_block(ui, md, &with_prefix(mark, spans), 13.0, false, *level as f32 * 11.0);
            }
            Block::Numbered { level, num, spans } => {
                md_block(ui, md, &with_prefix(&format!("{num}. "), spans), 13.0, false, *level as f32 * 11.0);
            }
            Block::Quote { depth, spans } => {
                let (x0, y0) = (ui.cursor().min.x, ui.cursor().min.y);
                md_block(ui, md, spans, 13.0, false, 14.0 * *depth as f32 + 4.0);
                let y1 = ui.min_rect().max.y;
                for d in 0..*depth {
                    let x = x0 + 3.0 + d as f32 * 11.0;
                    ui.painter()
                        .rect_filled(Rect::from_min_max(pos2(x, y0), pos2(x + 3.0, y1)), 1.0, GREY);
                }
            }
            Block::Code(lines) => {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for l in lines {
                        let l = if l.is_empty() { " " } else { l.as_str() };
                        ui.label(
                            egui::RichText::new(l)
                                .monospace()
                                .size(12.0)
                                .color(INK)
                                .background_color(CODE_BG),
                        );
                    }
                });
            }
            Block::Table { header, rows } => {
                egui::Grid::new(("md_table", bi))
                    .spacing(vec2(18.0, 4.0))
                    .show(ui, |ui| {
                        for cell in header {
                            md_block(ui, md, cell, 12.5, true, 0.0);
                        }
                        ui.end_row();
                        for row in rows {
                            for cell in row {
                                md_block(ui, md, cell, 12.5, false, 0.0);
                            }
                            ui.end_row();
                        }
                    });
            }
        }
    }
}

/// Parse (cached per text) and draw release notes.
pub fn render_text(ui: &mut egui::Ui, text: &str, md: &mut MdCtx) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    let key = h.finish();
    let blocks = md
        .parsed
        .entry(key)
        .or_insert_with(|| Rc::new(parse(text)))
        .clone();
    render(ui, &blocks, md);
}
