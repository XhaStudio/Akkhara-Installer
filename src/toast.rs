//! Pop-up notification cards in the bottom-right corner of the window:
//!   * no network connection
//!   * a new akk compiler version is available
//!   * a new installer version is available
//! Each card slides up with a little bounce, shows a timer bar along its bottom edge
//! (it pauses while the mouse is over the card) and closes itself when the time is up.

use crate::markdown::{mm_lines, mm_texture, MdCtx};
use crate::{rounded_path, BLUE, GREY, INK, ROW};
use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Rect, Sense, Shape, Stroke};
use std::sync::atomic::{AtomicU64, Ordering};

const W: f32 = 372.0; // card width
const CARD: Color32 = Color32::from_rgb(126, 203, 247);
const RED: Color32 = Color32::from_rgb(255, 92, 96);
const RED_EDGE: Color32 = Color32::from_rgb(80, 0, 8);
const YELLOW: Color32 = Color32::from_rgb(255, 214, 58);
const YELLOW_EDGE: Color32 = Color32::from_rgb(70, 50, 0);
const BAR_TRACK: Color32 = Color32::from_rgb(118, 169, 199);
const BAR_FILL: Color32 = Color32::from_rgb(75, 125, 153);

const BODY_PX: f32 = 11.5; // Burmese text size
const SLIDE_IN: f32 = 0.45; // seconds
const SLIDE_OUT: f32 = 0.30;

#[derive(Clone, PartialEq)]
pub enum Kind {
    Network,
    Compiler(String),  // new akk release tag
    Installer(String), // new installer release tag
    Installed(String), // this exact version is already installed
}

pub enum Action {
    Retry,
    Open(Kind),
}

pub struct Toast {
    id: u64,
    kind: Kind,
    age: f32,
    left: f32,
    total: f32,
    closing: Option<f32>,
}

impl Toast {
    pub fn new(kind: Kind) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let total = match kind {
            Kind::Network | Kind::Installed(_) => 6.0,
            _ => 8.0,
        };
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            kind,
            age: 0.0,
            left: total,
            total,
            closing: None,
        }
    }
}

impl Toast {
    /// Is this a still-open card of the given kind?
    pub fn is(&self, kind: &Kind) -> bool {
        self.closing.is_none() && self.kind == *kind
    }

    pub fn restart(&mut self) {
        self.left = self.total;
    }
}

fn ease_out_back(x: f32) -> f32 {
    let (c1, c3) = (1.70158_f32, 2.70158_f32);
    1.0 + c3 * (x - 1.0).powi(3) + c1 * (x - 1.0).powi(2)
}

fn outlined(p: &egui::Painter, pos: egui::Pos2, anchor: Align2, text: &str, size: f32, fill: Color32, edge: Color32) -> Rect {
    let font = FontId::monospace(size);
    for d in [vec2(-1.3, 0.0), vec2(1.3, 0.0), vec2(0.0, -1.3), vec2(0.0, 1.3), vec2(1.0, 1.0), vec2(-1.0, -1.0)] {
        p.text(pos + d, anchor, text, font.clone(), edge);
    }
    p.text(pos, anchor, text, font, fill)
}

fn draw_wifi_off(p: &egui::Painter, c: egui::Pos2, r: f32) {
    p.circle_stroke(c, r, Stroke::new(5.0_f32, RED));
    let o = c + vec2(0.0, r * 0.36);
    for rad in [r * 0.26, r * 0.52, r * 0.78] {
        let pts: Vec<egui::Pos2> = (0..=14)
            .map(|i| {
                let a = (-135.0_f32 + i as f32 * 90.0 / 14.0).to_radians();
                o + vec2(a.cos(), a.sin()) * rad
            })
            .collect();
        p.add(Shape::line(pts, Stroke::new(3.5_f32, RED)));
    }
    p.circle_filled(o, 2.6, RED);
    let (a, b) = (c + vec2(-r * 0.72, -r * 0.72), c + vec2(r * 0.72, r * 0.72));
    p.line_segment([a, b], Stroke::new(9.0_f32, CARD)); // gap around the slash
    p.line_segment([a, b], Stroke::new(4.5_f32, RED));
}

fn draw_bell(p: &egui::Painter, c: egui::Pos2) {
    let mut pts = Vec::new();
    for i in 0..=12 {
        let a = (180.0_f32 + i as f32 * 15.0).to_radians();
        pts.push(c + vec2(0.0, -3.0) + vec2(a.cos(), a.sin()) * 9.0);
    }
    pts.push(c + vec2(9.0, 4.0));
    pts.push(c + vec2(13.0, 9.0));
    pts.push(c + vec2(-13.0, 9.0));
    pts.push(c + vec2(-9.0, 4.0));
    p.add(Shape::convex_polygon(pts, INK, Stroke::NONE));
    p.circle_filled(c + vec2(0.0, -13.5), 2.2, INK);
    p.circle_filled(c + vec2(0.0, 13.5), 3.0, INK);
}

fn card_frame(p: &egui::Painter, r: Rect) {
    p.rect_filled(r, 22.0, CARD);
    p.extend(Shape::dashed_line(
        &rounded_path(r.shrink(2.5), 20.0),
        Stroke::new(4.0_f32, GREY),
        16.0,
        8.0,
    ));
}

fn timer_bar(p: &egui::Painter, card: Rect, frac: f32) {
    let track = Rect::from_min_max(pos2(card.min.x + 12.0, card.max.y - 20.0), pos2(card.max.x - 12.0, card.max.y - 9.0));
    p.rect_filled(track, 5.5, BAR_TRACK);
    if frac > 0.0 {
        let w = (track.width() * frac).max(track.height());
        p.rect_filled(Rect::from_min_size(track.min, vec2(w, track.height())), 5.5, BAR_FILL);
    }
}

fn x_mark(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    p.line_segment([c + vec2(-6.0, -6.0), c + vec2(6.0, 6.0)], Stroke::new(4.5_f32, col));
    p.line_segment([c + vec2(-6.0, 6.0), c + vec2(6.0, -6.0)], Stroke::new(4.5_f32, col));
}

/// (title, Burmese body, version tag) of the cards that have a bell badge.
fn update_text(kind: &Kind) -> (&'static str, String, String) {
    match kind {
        Kind::Compiler(tag) => (
            "New compiler version",
            "akk compiler ဗားရှင်းအသစ်တစ်ခု ရရှိနိုင်ပါပြီ။ နောက်ဆုံးပေါ် အင်္ဂါရပ်များနှင့် တိုးတက်မှုများကို ခံစားနိုင်ရန် ယခုပဲ အပ်ဒိတ်လုပ်ပါ။".to_string(),
            tag.clone(),
        ),
        Kind::Installer(tag) => (
            "New installer version",
            "Akkhara Installer ဗားရှင်းအသစ်တစ်ခု ရရှိနိုင်ပါပြီ။ ပိုမိုကောင်းမွန်သော installer ကို အသုံးပြုနိုင်ရန် ယခုပဲ အပ်ဒိတ်လုပ်ပါ။".to_string(),
            tag.clone(),
        ),
        Kind::Installed(tag) => (
            "Already installed",
            format!("ဤ version သည် install ပြီးသားဖြစ်သဖြင့် {tag} ကို install ရန် မလိုအပ်ပါ။"),
            tag.clone(),
        ),
        Kind::Network => ("", String::new(), String::new()),
    }
}

/// Advance timers and draw all toasts (oldest at the bottom). Returns an action if one was clicked.
pub fn show(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    full: Rect,
    toasts: &mut Vec<Toast>,
    md: &mut MdCtx,
    dt: f32,
) -> Option<Action> {
    let mut action = None;
    let pointer = ui.ctx().pointer_hover_pos();
    let right = full.max.x - 16.0;
    let mut y_bottom = full.max.y - 16.0;
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

    for t in toasts.iter_mut() {
        // ---- layout
        let is_net = matches!(t.kind, Kind::Network);
        let lines = if is_net {
            None
        } else {
            let (_, body, _) = update_text(&t.kind);
            Some(mm_lines(md, &body, BODY_PX, W - 32.0))
        };
        let line_h = BODY_PX * 1.7 + 4.0;
        let h = match &lines {
            None => 142.0,
            Some(l) => 92.0 + l.len() as f32 * line_h + 28.0,
        };
        let base = Rect::from_min_max(pos2(right - W, y_bottom - h), pos2(right, y_bottom));
        y_bottom -= h + 10.0;

        // ---- timers
        t.age += dt;
        let hovered = t.closing.is_none() && pointer.map_or(false, |pp| base.contains(pp));
        match &mut t.closing {
            None => {
                if !hovered {
                    t.left -= dt;
                }
                if t.left <= 0.0 {
                    t.closing = Some(0.0);
                }
            }
            Some(c) => *c += dt,
        }

        // ---- slide in (with a bounce) / slide out
        let slide_in = (1.0 - ease_out_back((t.age / SLIDE_IN).clamp(0.0, 1.0))) * (h + 30.0);
        let slide_out = t.closing.map_or(0.0, |c| (c / SLIDE_OUT).clamp(0.0, 1.0).powi(3) * (h + 30.0));
        let r = base.translate(vec2(0.0, slide_in + slide_out));

        // ---- interaction (the close button is registered last so it wins)
        let mut clicked_body = false;
        let mut clicked_x = false;
        let card = if is_net { r } else { Rect::from_min_max(pos2(r.min.x, r.min.y + 18.0), r.max) };
        let xr = Rect::from_center_size(pos2(card.max.x - 22.0, card.min.y + 22.0), vec2(30.0, 30.0));
        if t.closing.is_none() {
            let body = ui.interact(r, egui::Id::new(("toast_body", t.id)), Sense::click());
            let xresp = ui.interact(xr, egui::Id::new(("toast_x", t.id)), Sense::click());
            if body.hovered() || xresp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            clicked_x = xresp.clicked();
            clicked_body = body.clicked() && !clicked_x;
        }

        // ---- draw
        card_frame(p, card);
        let frac = (t.left / t.total).clamp(0.0, 1.0);
        match (&t.kind, &lines) {
            (Kind::Network, _) => {
                draw_wifi_off(p, pos2(card.center().x, card.min.y + 42.0), 26.0);
                outlined(p, pos2(card.center().x, card.min.y + 88.0), Align2::CENTER_CENTER, "We couldn't connect to the network.", 10.5, RED, RED_EDGE);
                outlined(p, pos2(card.center().x, card.min.y + 106.0), Align2::CENTER_CENTER, "Please check your internet connection and try again!", 10.5, RED, RED_EDGE);
            }
            (kind, Some(lines)) => {
                let (title, _, tag) = update_text(kind);
                let cy = r.min.y + 62.0;
                let tr = outlined(p, pos2(card.min.x + 16.0, cy), Align2::LEFT_CENTER, title, 13.0, YELLOW, YELLOW_EDGE);
                let vr = p.text(pos2(tr.max.x + 12.0, cy), Align2::LEFT_CENTER, &tag, FontId::monospace(18.0), BLUE);
                outlined(p, pos2(vr.max.x + 10.0, cy), Align2::LEFT_CENTER, "!!", 14.0, YELLOW, YELLOW_EDGE);
                let mut y = r.min.y + 82.0;
                for l in lines.iter() {
                    if let Some((id, sz)) = mm_texture(md, l, BODY_PX) {
                        p.image(id, Rect::from_min_size(pos2(card.min.x + 16.0, y), sz), uv, Color32::WHITE);
                        y += line_h;
                    }
                }
                // bell badge overlapping the top-left corner
                let badge = Rect::from_min_size(pos2(r.min.x + 6.0, r.min.y), vec2(42.0, 42.0));
                p.rect(badge.shrink(2.0), 12.0, ROW, Stroke::new(4.0_f32, GREY));
                draw_bell(p, badge.center());
            }
            _ => {}
        }
        timer_bar(p, card, frac);
        x_mark(p, xr.center(), if clicked_x { BLUE } else { INK });

        // ---- results
        if clicked_x {
            t.closing = Some(0.0);
        } else if clicked_body {
            t.closing = Some(0.0);
            action = match &t.kind {
                Kind::Network => Some(Action::Retry),
                Kind::Installed(_) => None, // just a notice: clicking only closes it
                other => Some(Action::Open(other.clone())),
            };
        }
    }
    toasts.retain(|t| t.closing.map_or(true, |c| c < SLIDE_OUT));
    action
}
