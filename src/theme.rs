//! Paleta, estilo global y todos los iconos (dibujados con vectores: sin fuentes ni imágenes).
use crate::fsys::Kind;
use egui::{pos2, vec2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke};

#[derive(Clone, Copy)]
pub struct Pal {
    pub bg: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub subtle: Color32,
    pub accent: Color32,
    pub sel: Color32,
    pub page: Color32,
}

impl Pal {
    /// Mezcla lineal de dos paletas (t = 0 → `self`, t = 1 → `o`), para animar el cambio de tema.
    pub fn lerp(&self, o: &Pal, t: f32) -> Pal {
        let m = |a: Color32, b: Color32| {
            let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
            Color32::from_rgba_premultiplied(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()), f(a.a(), b.a()))
        };
        Pal {
            bg: m(self.bg, o.bg),
            panel: m(self.panel, o.panel),
            surface: m(self.surface, o.surface),
            hover: m(self.hover, o.hover),
            border: m(self.border, o.border),
            text: m(self.text, o.text),
            subtle: m(self.subtle, o.subtle),
            accent: m(self.accent, o.accent),
            sel: m(self.sel, o.sel),
            page: m(self.page, o.page),
        }
    }
}

pub fn palette(dark: bool) -> Pal {
    let c = |r, g, b| Color32::from_rgb(r, g, b);
    if dark {
        Pal {
            bg: c(24, 26, 32),
            panel: c(30, 33, 41),
            surface: c(39, 43, 54),
            hover: c(50, 55, 69),
            border: c(52, 57, 71),
            text: c(231, 233, 240),
            subtle: c(137, 145, 165),
            accent: c(94, 140, 255),
            sel: Color32::from_rgba_unmultiplied(94, 140, 255, 64),
            page: c(236, 239, 246),
        }
    } else {
        Pal {
            bg: c(246, 247, 250),
            panel: c(236, 239, 245),
            surface: c(255, 255, 255),
            hover: c(224, 229, 240),
            border: c(210, 216, 228),
            text: c(30, 34, 44),
            subtle: c(105, 114, 135),
            accent: c(52, 106, 240),
            sel: Color32::from_rgba_unmultiplied(52, 106, 240, 50),
            page: c(255, 255, 255),
        }
    }
}

impl Pal {
    pub fn colors(&self) -> [[u8; 3]; 9] {
        let a = |c: Color32| [c.r(), c.g(), c.b()];
        [a(self.bg), a(self.panel), a(self.surface), a(self.hover), a(self.border), a(self.text), a(self.subtle), a(self.accent), a(self.page)]
    }
}

/// Paleta de un tema propio; la selección se deriva del acento.
pub fn palette_of(th: &crate::config::CustomTheme) -> Pal {
    let c = |i: usize| Color32::from_rgb(th.colors[i][0], th.colors[i][1], th.colors[i][2]);
    let ac = th.colors[7];
    Pal {
        bg: c(0),
        panel: c(1),
        surface: c(2),
        hover: c(3),
        border: c(4),
        text: c(5),
        subtle: c(6),
        accent: c(7),
        sel: Color32::from_rgba_unmultiplied(ac[0], ac[1], ac[2], if th.dark { 64 } else { 50 }),
        page: c(8),
    }
}

/// Paleta vigente: la del tema propio activo si coincide con el modo, o la de serie.
pub fn resolve(cfg: &crate::config::Config, dark: bool) -> Pal {
    if let Some(th) = cfg.custom_themes.iter().find(|t| t.name == cfg.theme_custom) {
        if th.dark == dark {
            return palette_of(th);
        }
    }
    palette(dark)
}

pub fn apply_style(ctx: &egui::Context, p: &Pal, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.panel;
    v.override_text_color = Some(p.text);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.selection.bg_fill = p.sel;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(9);
    }
    v.widgets.inactive.bg_fill = p.surface;
    v.widgets.inactive.weak_bg_fill = p.surface;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.active.bg_fill = p.hover;
    v.widgets.active.weak_bg_fill = p.hover;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.animation_time = 0.0;
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.interaction.tooltip_delay = 0.4;
    });
}

// ---------------------------------------------------------------------------
// Glifos de interfaz
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Back,
    Forward,
    Up,
    Refresh,
    Grid,
    List,
    Star,
    StarFilled,
    Folder,
    Network,
    Bookmark,
    BookmarkFilled,
    Gear,
    Home,
    Sun,
    Moon,
    Eye,
    EyeOff,
    FolderPlus,
    Desktop,
    Document,
    Download,
    Picture,
    Music,
    Video,
    Drive,
    Close,
    Menu,
    Minimize,
    Maximize,
    Restore,
    Plus,
    Check,
    More,
    Trash,
    Clock,
    Columns,
}

/// Trazo con terminaciones redondeadas (egui no las ofrece): se añaden discos en los extremos.
fn path(p: &Painter, pts: &[Pos2], closed: bool, st: Stroke) {
    if closed {
        p.add(Shape::closed_line(pts.to_vec(), st));
    } else {
        p.add(Shape::line(pts.to_vec(), st));
        let r = st.width / 2.0;
        if let (Some(a), Some(b)) = (pts.first(), pts.last()) {
            p.circle_filled(*a, r, st.color);
            p.circle_filled(*b, r, st.color);
        }
    }
}

fn arc(c: Pos2, r: f32, a0: f32, a1: f32, n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect()
}

fn star_points(c: Pos2, r: f32) -> Vec<Pos2> {
    (0..10)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let rr = if i % 2 == 0 { r } else { r * 0.47 };
            c + vec2(a.cos(), a.sin()) * rr
        })
        .collect()
}

/// Dibuja un glifo en una rejilla de 24 unidades (zona útil de ±10) centrada en `c`.
/// Trazo uniforme, esquinas y extremos redondeados, grosor proporcional al tamaño.
pub fn draw_glyph(p: &Painter, c: Pos2, s: f32, g: Glyph, col: Color32) {
    let u = s / 24.0;
    let w = (s / 12.0).max(1.5);
    let st = Stroke::new(w, col);
    let q = |x: f32, y: f32| c + vec2(x * u, y * u);
    let rr = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(q(x0, y0), q(x1, y1));
    let rad = |r: f32| CornerRadius::same((r * u).round().clamp(0.0, 255.0) as u8);
    let tau = std::f32::consts::TAU;
    match g {
        Glyph::Back => {
            path(p, &[q(9.0, 0.0), q(-9.0, 0.0)], false, st);
            path(p, &[q(-3.0, -6.0), q(-9.0, 0.0), q(-3.0, 6.0)], false, st);
        }
        Glyph::Forward => {
            path(p, &[q(-9.0, 0.0), q(9.0, 0.0)], false, st);
            path(p, &[q(3.0, -6.0), q(9.0, 0.0), q(3.0, 6.0)], false, st);
        }
        Glyph::Up => {
            path(p, &[q(0.0, 9.0), q(0.0, -9.0)], false, st);
            path(p, &[q(-6.0, -3.0), q(0.0, -9.0), q(6.0, -3.0)], false, st);
        }
        Glyph::Refresh => {
            // arco de 280° en sentido horario con punta de flecha rellena al final
            let a0 = 30f32.to_radians();
            let a1 = a0 + 280f32.to_radians();
            let pts: Vec<Pos2> = arc(c, 8.0 * u, a0, a1, 28);
            path(p, &pts, false, st);
            let e = *pts.last().unwrap();
            let tan = vec2(-a1.sin(), a1.cos());
            let nor = vec2(a1.cos(), a1.sin());
            let tip = e + tan * 5.0 * u;
            p.add(Shape::convex_polygon(
                vec![tip, e + nor * 4.2 * u, e - nor * 4.2 * u],
                col,
                Stroke::new(w * 0.5, col),
            ));
        }
        Glyph::Grid => {
            for (x, y) in [(-9.0, -9.0), (1.0, -9.0), (-9.0, 1.0), (1.0, 1.0)] {
                p.rect_stroke(rr(x, y, x + 8.0, y + 8.0), rad(2.5), st, egui::StrokeKind::Inside);
            }
        }
        Glyph::List => {
            // filas con miniatura + línea (vista de lista), distinto del icono de menú
            for y in [-6.5, 0.0, 6.5] {
                p.rect_filled(rr(-10.0, y - 2.4, -5.2, y + 2.4), rad(1.4), col);
                path(p, &[q(-1.0, y), q(10.0, y)], false, st);
            }
        }
        Glyph::Star | Glyph::StarFilled => {
            let ctr = q(0.0, 0.6);
            let pts = star_points(ctr, 10.0 * u);
            if g == Glyph::StarFilled {
                for i in 0..10 {
                    p.add(Shape::convex_polygon(vec![ctr, pts[i], pts[(i + 1) % 10]], col, Stroke::new(0.6, col)));
                }
            }
            p.add(Shape::closed_line(pts, st));
        }
        Glyph::Network => {
            // globo: círculo, ecuador y meridiano
            p.circle_stroke(c, 8.6 * u, st);
            path(p, &[q(-8.6, 0.0), q(8.6, 0.0)], false, st);
            let mer: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 16.0;
                    q(a.cos() * 4.2, a.sin() * 8.6)
                })
                .collect();
            path(p, &mer, false, st);
            let mer2: Vec<Pos2> = mer.iter().map(|m| c + (c - *m)).collect();
            path(p, &mer2, false, st);
        }
        Glyph::Folder => {
            p.add(Shape::closed_line(
                vec![q(-9.5, -6.5), q(-3.5, -6.5), q(-1.2, -3.8), q(9.5, -3.8), q(9.5, 7.5), q(-9.5, 7.5)],
                st,
            ));
        }
        Glyph::Bookmark | Glyph::BookmarkFilled => {
            if g == Glyph::BookmarkFilled {
                p.add(Shape::convex_polygon(vec![q(-5.5, -9.0), q(0.0, -9.0), q(0.0, 4.5), q(-5.5, 9.5)], col, Stroke::new(0.6, col)));
                p.add(Shape::convex_polygon(vec![q(5.5, -9.0), q(0.0, -9.0), q(0.0, 4.5), q(5.5, 9.5)], col, Stroke::new(0.6, col)));
            }
            p.add(Shape::closed_line(vec![q(-5.5, -9.0), q(5.5, -9.0), q(5.5, 9.5), q(0.0, 4.5), q(-5.5, 9.5)], st));
        }
        Glyph::Gear => {
            // engranaje de 8 dientes con base redondeada y círculo central
            let n = 8;
            let mut pts = Vec::new();
            for i in 0..n {
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 * tau / n as f32;
                let half_tip = 0.17;
                let half_base = 0.30;
                pts.push(c + vec2((a - half_base).cos(), (a - half_base).sin()) * 7.3 * u);
                pts.push(c + vec2((a - half_tip).cos(), (a - half_tip).sin()) * 9.9 * u);
                pts.push(c + vec2((a + half_tip).cos(), (a + half_tip).sin()) * 9.9 * u);
                pts.push(c + vec2((a + half_base).cos(), (a + half_base).sin()) * 7.3 * u);
                // arco redondeado hasta el siguiente diente
                let a0 = a + half_base;
                let a1 = a + tau / n as f32 - half_base;
                for k in 1..4 {
                    let aa = a0 + (a1 - a0) * k as f32 / 4.0;
                    pts.push(c + vec2(aa.cos(), aa.sin()) * 7.3 * u);
                }
            }
            p.add(Shape::closed_line(pts, st));
            p.circle_stroke(c, 3.0 * u, st);
        }
        Glyph::Home => {
            path(p, &[q(-9.5, -1.5), q(0.0, -9.5), q(9.5, -1.5)], false, st);
            path(p, &[q(-6.0, -4.3), q(-6.0, 9.0), q(6.0, 9.0), q(6.0, -4.3)], false, st);
            path(p, &[q(-2.2, 9.0), q(-2.2, 3.0), q(2.2, 3.0), q(2.2, 9.0)], false, st);
        }
        Glyph::Sun => {
            p.circle_stroke(c, 4.6 * u, st);
            for i in 0..8 {
                let a = i as f32 * tau / 8.0;
                let d = vec2(a.cos(), a.sin());
                path(p, &[c + d * 7.6 * u, c + d * 10.0 * u], false, st);
            }
        }
        Glyph::Moon => {
            // creciente: arco exterior + arco interior desplazado
            let (cr, rr_) = (q(-0.5, 0.5), 9.0 * u);
            let off = vec2(4.6, -3.8) * u;
            let (ci, ri) = (cr + off, 7.4 * u);
            let away = (-off).angle();
            let outside = |a: f32| (cr + vec2(a.cos(), a.sin()) * rr_ - ci).length() >= ri;
            let (mut s_a, mut e_a) = (away, away);
            while outside(e_a) && e_a - away < tau {
                e_a += 0.03;
            }
            while outside(s_a) && away - s_a < tau {
                s_a -= 0.03;
            }
            let mut pts = arc(cr, rr_, s_a, e_a, 40);
            let toward = off.angle() + std::f32::consts::PI; // lado del círculo interior que mira al centro
            let mut inner: Vec<Pos2> = Vec::new();
            let mut d = -std::f32::consts::PI;
            while d <= std::f32::consts::PI {
                let pt = ci + vec2((toward + d).cos(), (toward + d).sin()) * ri;
                if (pt - cr).length() <= rr_ + 0.01 {
                    inner.push(pt);
                }
                d += 0.05;
            }
            if let (Some(last), Some(f), Some(l)) = (pts.last().copied(), inner.first().copied(), inner.last().copied()) {
                if (last - f).length() > (last - l).length() {
                    inner.reverse();
                }
            }
            pts.extend(inner);
            p.add(Shape::closed_line(pts, st));
        }
        Glyph::Eye | Glyph::EyeOff => {
            let top: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let x = -10.0 + 20.0 * i as f32 / 16.0;
                    q(x, -7.5 * (1.0 - (x / 10.0) * (x / 10.0)))
                })
                .collect();
            let mut shape = top.clone();
            shape.extend(top.iter().rev().map(|a| pos2(a.x, 2.0 * c.y - a.y)));
            p.add(Shape::closed_line(shape, st));
            p.circle_stroke(c, 3.2 * u, st);
            if g == Glyph::EyeOff {
                path(p, &[q(-8.0, 8.0), q(8.0, -8.0)], false, st);
            }
        }
        Glyph::FolderPlus => {
            p.add(Shape::closed_line(
                vec![q(-10.0, -7.0), q(-3.5, -7.0), q(-1.0, -4.0), q(10.0, -4.0), q(10.0, 8.0), q(-10.0, 8.0)],
                st,
            ));
            path(p, &[q(-3.5, 2.5), q(3.5, 2.5)], false, st);
            path(p, &[q(0.0, -1.0), q(0.0, 6.0)], false, st);
        }
        Glyph::Desktop => {
            p.rect_stroke(rr(-10.0, -8.0, 10.0, 4.0), rad(2.5), st, egui::StrokeKind::Inside);
            path(p, &[q(0.0, 4.0), q(0.0, 8.0)], false, st);
            path(p, &[q(-4.5, 8.5), q(4.5, 8.5)], false, st);
        }
        Glyph::Document => {
            p.add(Shape::closed_line(
                vec![q(-7.0, -10.0), q(2.0, -10.0), q(7.0, -5.0), q(7.0, 10.0), q(-7.0, 10.0)],
                st,
            ));
            path(p, &[q(2.0, -10.0), q(2.0, -5.0), q(7.0, -5.0)], false, st);
            path(p, &[q(-3.0, 1.5), q(3.0, 1.5)], false, st);
            path(p, &[q(-3.0, 5.5), q(3.0, 5.5)], false, st);
        }
        Glyph::Download => {
            path(p, &[q(0.0, -9.0), q(0.0, 3.0)], false, st);
            path(p, &[q(-5.5, -2.5), q(0.0, 3.0), q(5.5, -2.5)], false, st);
            path(p, &[q(-9.0, 4.0), q(-9.0, 9.0), q(9.0, 9.0), q(9.0, 4.0)], false, st);
        }
        Glyph::Picture => {
            p.rect_stroke(rr(-10.0, -8.0, 10.0, 8.0), rad(3.0), st, egui::StrokeKind::Inside);
            path(p, &[q(-9.5, 5.5), q(-3.5, -1.0), q(1.0, 3.5), q(4.0, 0.5), q(9.5, 5.5)], false, st);
            p.circle_stroke(q(5.0, -3.5), 1.8 * u, st);
        }
        Glyph::Music => {
            p.circle_filled(q(-1.5, 6.0), 3.4 * u, col);
            path(p, &[q(1.9, 6.0), q(1.9, -9.0)], false, st);
            path(p, &[q(1.9, -9.0), q(7.0, -6.5), q(8.5, -2.5)], false, st);
        }
        Glyph::Video => {
            p.rect_stroke(rr(-10.0, -7.0, 10.0, 7.0), rad(3.0), st, egui::StrokeKind::Inside);
            p.add(Shape::convex_polygon(
                vec![q(-2.4, -3.8), q(4.4, 0.0), q(-2.4, 3.8)],
                col,
                Stroke::new(w * 0.6, col),
            ));
        }
        Glyph::Drive => {
            p.rect_stroke(rr(-10.0, -5.5, 10.0, 7.0), rad(3.0), st, egui::StrokeKind::Inside);
            path(p, &[q(-10.0, 0.5), q(10.0, 0.5)], false, st);
            p.circle_filled(q(5.8, 3.8), 1.3 * u, col);
        }
        Glyph::Close => {
            path(p, &[q(-6.0, -6.0), q(6.0, 6.0)], false, st);
            path(p, &[q(-6.0, 6.0), q(6.0, -6.0)], false, st);
        }
        Glyph::Menu => {
            for y in [-6.0, 0.0, 6.0] {
                path(p, &[q(-9.0, y), q(9.0, y)], false, st);
            }
        }
        Glyph::Minimize => {
            path(p, &[q(-8.0, 3.0), q(8.0, 3.0)], false, st);
        }
        Glyph::Maximize => {
            p.rect_stroke(rr(-8.0, -8.0, 8.0, 8.0), rad(2.5), st, egui::StrokeKind::Inside);
        }
        Glyph::Restore => {
            p.rect_stroke(rr(-8.0, -3.0, 3.0, 8.0), rad(2.5), st, egui::StrokeKind::Inside);
            path(p, &[q(-3.0, -3.0), q(-3.0, -6.0), q(0.5, -8.0), q(8.0, -8.0), q(8.0, 0.0), q(3.0, 0.0)], false, st);
        }
        Glyph::Plus => {
            path(p, &[q(-8.0, 0.0), q(8.0, 0.0)], false, st);
            path(p, &[q(0.0, -8.0), q(0.0, 8.0)], false, st);
        }
        Glyph::More => {
            for y in [-6.0, 0.0, 6.0] {
                p.circle_filled(q(0.0, y), 1.9, st.color);
            }
        }
        Glyph::Columns => {
            path(p, &[q(-8.0, -7.0), q(8.0, -7.0), q(8.0, 7.0), q(-8.0, 7.0)], true, st);
            path(p, &[q(-2.7, -7.0), q(-2.7, 7.0)], false, st);
            path(p, &[q(2.7, -7.0), q(2.7, 7.0)], false, st);
        }
        Glyph::Trash => {
            path(p, &[q(-8.0, -5.0), q(8.0, -5.0)], false, st);
            path(p, &[q(-2.5, -5.0), q(-2.5, -8.0), q(2.5, -8.0), q(2.5, -5.0)], false, st);
            path(p, &[q(-6.0, -5.0), q(-5.0, 8.0), q(5.0, 8.0), q(6.0, -5.0)], false, st);
            path(p, &[q(-2.0, -1.5), q(-2.0, 4.5)], false, st);
            path(p, &[q(2.0, -1.5), q(2.0, 4.5)], false, st);
        }
        Glyph::Clock => {
            p.circle_stroke(q(0.0, 0.0), 8.0, st);
            path(p, &[q(0.0, -4.5), q(0.0, 0.0), q(4.0, 2.5)], false, st);
        }
        Glyph::Check => {
            path(p, &[q(-8.0, -8.0), q(8.0, -8.0), q(8.0, 8.0), q(-8.0, 8.0)], true, st);
            path(p, &[q(-4.0, 0.5), q(-1.0, 4.0), q(4.5, -3.5)], false, st);
        }
    }
}

// ---------------------------------------------------------------------------
// Iconos de archivo
// ---------------------------------------------------------------------------

pub fn kind_color(k: Kind) -> Color32 {
    match k {
        Kind::Folder => Color32::from_rgb(244, 190, 80),
        Kind::Image => Color32::from_rgb(76, 175, 120),
        Kind::Video => Color32::from_rgb(226, 98, 98),
        Kind::Audio => Color32::from_rgb(170, 110, 225),
        Kind::Text => Color32::from_rgb(86, 156, 235),
        Kind::Archive => Color32::from_rgb(222, 150, 70),
        Kind::Doc => Color32::from_rgb(226, 120, 90),
        Kind::Other => Color32::from_rgb(130, 140, 160),
    }
}

pub fn draw_folder(p: &Painter, rect: Rect) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let w = s * 0.92;
    let h = s * 0.74;
    let body = Rect::from_center_size(c + vec2(0.0, s * 0.04), vec2(w, h));
    let rad = CornerRadius::same((s * 0.1) as u8);
    let tab = Rect::from_min_size(body.min + vec2(0.0, -s * 0.09), vec2(w * 0.42, s * 0.2));
    p.rect_filled(tab, rad, Color32::from_rgb(214, 150, 40));
    p.rect_filled(body, rad, Color32::from_rgb(222, 162, 52));
    let front = Rect::from_min_max(body.min + vec2(0.0, h * 0.2), body.max);
    p.rect_filled(front, rad, Color32::from_rgb(247, 196, 92));
}

pub fn draw_file(p: &Painter, rect: Rect, kind: Kind, ext: &str, pal: &Pal) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let w = s * 0.68;
    let h = s * 0.88;
    let page = Rect::from_center_size(c, vec2(w, h));
    let fold = w * 0.3;
    let col = kind_color(kind);
    let rad = (s * 0.07) as u8;
    p.rect_filled(page, CornerRadius { nw: rad, ne: 0, sw: rad, se: rad }, pal.page);
    // esquina doblada
    let tr = page.right_top();
    p.add(Shape::convex_polygon(
        vec![tr + vec2(-fold, 0.0), tr + vec2(0.0, fold), tr + vec2(-fold, fold)],
        col.gamma_multiply(0.55),
        Stroke::NONE,
    ));
    // banda de color con extensión
    let band = Rect::from_min_max(pos2(page.left(), page.bottom() - h * 0.26), page.right_bottom());
    p.rect_filled(band, CornerRadius { nw: 0, ne: 0, sw: rad, se: rad }, col);
    if s >= 52.0 && !ext.is_empty() {
        let t: String = ext.chars().take(4).collect::<String>().to_uppercase();
        p.text(band.center(), egui::Align2::CENTER_CENTER, t, egui::FontId::proportional((s * 0.13).clamp(8.0, 15.0)), Color32::WHITE);
    }
    // glifo central
    let gc = pos2(c.x, page.top() + h * 0.42);
    let gs = w * 0.5;
    match kind {
        Kind::Image => draw_glyph(p, gc, gs, Glyph::Picture, col),
        Kind::Audio => draw_glyph(p, gc, gs, Glyph::Music, col),
        Kind::Video => draw_glyph(p, gc, gs, Glyph::Video, col),
        Kind::Doc | Kind::Text => draw_glyph(p, gc, gs, Glyph::Document, col),
        Kind::Archive => {
            let st = Stroke::new((s / 18.0).max(1.2), col);
            for i in 0..4 {
                let y = gc.y - gs * 0.45 + i as f32 * gs * 0.3;
                let x = gc.x + if i % 2 == 0 { -gs * 0.08 } else { gs * 0.08 };
                p.line_segment([pos2(x - gs * 0.12, y), pos2(x + gs * 0.12, y)], st);
            }
        }
        _ => {}
    }
}

