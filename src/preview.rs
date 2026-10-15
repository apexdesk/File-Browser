//! Vista rápida (Espacio): imágenes a pantalla casi completa con zoom/pan, texto virtualizado,
//! carpetas y ficha de información. Un único hilo de fondo; sólo se atiende la última petición.
use crate::fsys::{fmt_time, human_size, Kind};
use crate::i18n::{t, tf};
use crate::theme::{self, Glyph, Pal};
use crate::thumbs::{decode_image, Decoded};
use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};

const MAX_IMG: u32 = 3072;

struct Job {
    generation: u64,
    path: PathBuf,
    kind: Kind,
}

enum Raw {
    Image(Decoded),
    Text(Vec<String>, bool),
    Folder(usize, Vec<String>),
    Archive(crate::archive::Listing),
    Binary,
    Err(String),
}

pub enum Body {
    Loading,
    Image(egui::TextureHandle),
    Text { lines: Vec<String>, truncated: bool },
    Folder { count: usize, names: Vec<String> },
    Archive { rows: Vec<(String, u64, bool)>, truncated: bool },
    Info,
    Error(String),
}

pub struct Preview {
    generation: u64,
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub pos_label: String,
    pub body: Body,
    pub thumb: Option<egui::TextureHandle>,
    pub player: Option<crate::video::Player>,
    zoom: f32,
    pan: egui::Vec2,
    scroll: f32,
    /// Última posición conocida del puntero (al soltar un dedo ya no hay posición).
    last_pos: Option<Pos2>,
}

pub enum Action {
    None,
    Close,
    /// Anterior (-1) / siguiente (1) archivo.
    Step(isize),
    /// Abrir con otra aplicación.
    Open,
}

pub struct Worker {
    slot: Arc<(Mutex<Option<Job>>, Condvar)>,
    rx: Receiver<(u64, Raw)>,
    generation: u64,
}

impl Worker {
    pub fn new(ctx: &egui::Context) -> Worker {
        let slot: Arc<(Mutex<Option<Job>>, Condvar)> = Arc::new((Mutex::new(None), Condvar::new()));
        let (tx, rx) = channel();
        let (s, ctx) = (slot.clone(), ctx.clone());
        std::thread::spawn(move || loop {
            let job = {
                let mut g = s.0.lock().unwrap();
                loop {
                    if let Some(j) = g.take() {
                        break j;
                    }
                    g = s.1.wait(g).unwrap();
                }
            };
            let raw = load(&job);
            if tx.send((job.generation, raw)).is_err() {
                return;
            }
            ctx.request_repaint();
        });
        Worker { slot, rx, generation: 0 }
    }

    fn submit(&mut self, path: PathBuf, kind: Kind) -> u64 {
        self.generation += 1;
        *self.slot.0.lock().unwrap() = Some(Job { generation: self.generation, path, kind });
        self.slot.1.notify_one();
        self.generation
    }
}

fn fmt_secs(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn load(job: &Job) -> Raw {
    match job.kind {
        Kind::Folder => match std::fs::read_dir(&job.path) {
            Ok(rd) => {
                let mut n = 0usize;
                let mut names = Vec::new();
                for e in rd.flatten() {
                    n += 1;
                    if names.len() < 60 {
                        names.push(e.file_name().to_string_lossy().into_owned());
                    }
                    if n >= 200_000 {
                        break;
                    }
                }
                names.sort_by(|a, b| crate::fsys::natural_cmp(a, b));
                Raw::Folder(n, names)
            }
            Err(e) => Raw::Err(e.to_string()),
        },
        Kind::Archive => match crate::archive::list(&job.path, 3000, std::time::Duration::from_millis(700)) {
            Ok(l) if !l.items.is_empty() => Raw::Archive(l),
            Ok(_) => Raw::Binary,
            Err(_) => Raw::Binary,
        },
        Kind::Image | Kind::Video => match decode_image(&job.path, MAX_IMG) {
            Some(d) => Raw::Image(d),
            None => Raw::Binary,
        },
        Kind::Text | Kind::Other => {
            let Ok(f) = std::fs::File::open(&job.path) else {
                return Raw::Err(t("No se pudo abrir").into());
            };
            let mut buf = Vec::new();
            let _ = f.take(256 * 1024).read_to_end(&mut buf);
            if buf[..buf.len().min(4096)].contains(&0) {
                return Raw::Binary;
            }
            let txt = String::from_utf8_lossy(&buf);
            let mut lines: Vec<String> = Vec::new();
            let mut truncated = buf.len() >= 256 * 1024;
            for l in txt.lines() {
                if lines.len() >= 6000 {
                    truncated = true;
                    break;
                }
                let l = l.replace('\t', "    ");
                lines.push(if l.chars().count() > 500 { l.chars().take(500).collect() } else { l });
            }
            Raw::Text(lines, truncated)
        }
        _ => Raw::Binary,
    }
}

impl Preview {
    pub fn open(
        w: &mut Worker,
        path: PathBuf,
        name: String,
        kind: Kind,
        size: u64,
        mtime: i64,
        pos_label: String,
        thumb: Option<egui::TextureHandle>,
    ) -> Preview {
        let generation = w.submit(path.clone(), kind);
        Preview {
            generation,
            name,
            path,
            kind,
            size,
            mtime,
            pos_label,
            body: Body::Loading,
            thumb,
            player: None,
            last_pos: None,
            zoom: 1.0,
            pan: vec2(0.0, 0.0),
            scroll: 0.0,
        }
    }

    pub fn player_active(&self) -> bool {
        self.player.as_ref().map(|p| p.playing()).unwrap_or(false)
    }

    /// Crea el reproductor si hace falta y alterna reproducción/pausa.
    pub fn toggle_play(&mut self, ctx: &egui::Context) {
        if self.kind != Kind::Video || !crate::video::have_ffmpeg() {
            return;
        }
        let path = self.path.clone();
        let pl = self.player.get_or_insert_with(|| crate::video::Player::new(ctx, &path));
        pl.toggle();
    }

    pub fn seek_by(&mut self, d: f64) {
        if let Some(p) = &mut self.player {
            p.seek_by(d);
        }
    }

    pub fn poll(&mut self, w: &mut Worker, ctx: &egui::Context) {
        if let Some(p) = &mut self.player {
            p.poll();
            if p.playing() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        while let Ok((generation, raw)) = w.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.body = match raw {
                Raw::Image(d) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([d.w, d.h], &d.rgba);
                    Body::Image(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR))
                }
                Raw::Text(lines, truncated) => Body::Text { lines, truncated },
                Raw::Folder(count, names) => Body::Folder { count, names },
                Raw::Archive(l) => {
                    let mut rows: Vec<(String, u64, bool)> = l.items.into_iter().map(|i| (i.name, i.size, i.dir)).collect();
                    rows.sort_by(|a, b| crate::fsys::natural_cmp(&a.0, &b.0));
                    Body::Archive { rows, truncated: l.truncated }
                }
                Raw::Binary => Body::Info,
                Raw::Err(e) => Body::Error(e),
            };
        }
    }

    /// Dibuja el overlay. Devuelve la acción solicitada con el ratón.
    pub fn show(&mut self, ctx: &egui::Context, pal: &Pal, windowed: Option<bool>) -> Action {
        let screen = ctx.content_rect();
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("preview_layer"));
        let p = ctx.layer_painter(layer);
        let (card, rad) = if let Some(rounded) = windowed {
            // ventana propia sin decoración del sistema: la tarjeta ocupa toda la ventana
            let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            let rad = if rounded && !max { CornerRadius::same(14) } else { CornerRadius::ZERO };
            p.rect_filled(screen, rad, pal.panel);
            p.rect_stroke(screen, rad, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
            (screen, rad)
        } else {
            p.rect_filled(screen, 0.0, Color32::from_black_alpha(175));
            let mx = (screen.width() * 0.06).clamp(24.0, 160.0);
            let my = (screen.height() * 0.06).clamp(20.0, 80.0);
            let card = screen.shrink2(vec2(mx, my));
            let rad = CornerRadius::same(18);
            p.rect_filled(card.translate(vec2(0.0, 6.0)).expand(2.0), rad, Color32::from_black_alpha(70));
            p.rect_filled(card, rad, pal.panel);
            p.rect_stroke(card, rad, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
            (card, rad)
        };
        let _ = rad;

        // Cabecera
        let head_h = 58.0;
        let head = Rect::from_min_size(card.min, vec2(card.width(), head_h));
        let icon_r = Rect::from_center_size(pos2(head.left() + 34.0, head.center().y), vec2(34.0, 34.0));
        if self.kind == Kind::Folder {
            theme::draw_folder(&p, icon_r);
        } else {
            let ext = self.name.rsplit_once('.').map(|x| x.1).unwrap_or("");
            theme::draw_file(&p, icon_r, self.kind, ext, pal);
        }
        let tx = head.left() + 62.0;
        let maxw = head.width() - 62.0 - 70.0 - if cfg!(target_os = "android") { 120.0 } else { 0.0 };
        let mut title: String = self.name.clone();
        let g = p.layout_no_wrap(title.clone(), FontId::proportional(16.0), pal.text);
        if g.size().x > maxw {
            let keep = ((maxw / g.size().x) * title.chars().count() as f32) as usize;
            title = title.chars().take(keep.saturating_sub(2)).collect::<String>() + "…";
        }
        p.text(pos2(tx, head.center().y - 9.0), Align2::LEFT_CENTER, title, FontId::proportional(16.0), pal.text);
        let sub = if self.kind == Kind::Folder {
            format!("{}  ·  {}", self.kind.label(), self.pos_label)
        } else {
            format!("{}  ·  {}  ·  {}  ·  {}", self.kind.label(), human_size(self.size), fmt_time(self.mtime), self.pos_label)
        };
        p.text(pos2(tx, head.center().y + 11.0), Align2::LEFT_CENTER, sub, FontId::proportional(12.0), pal.subtle);

        let live = ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()));
        if live.is_some() {
            self.last_pos = live;
        }
        let clicked = ctx.input(|i| i.pointer.primary_clicked());
        let pointer = live.or(if clicked { self.last_pos } else { None });
        let touch = cfg!(target_os = "android") || ctx.input(|i| i.any_touches());
        let mut action = Action::None;
        let close_r = Rect::from_center_size(pos2(head.right() - 32.0, head.center().y), vec2(34.0, 34.0));
        let over_close = pointer.map(|q| close_r.contains(q)).unwrap_or(false);
        if over_close {
            p.rect_filled(close_r, CornerRadius::same(10), pal.hover);
        }
        theme::draw_glyph(&p, close_r.center(), 16.0, Glyph::Close, pal.text);
        if over_close && clicked {
            action = Action::Close;
        }
        if touch {
            // «Abrir con…» junto al cierre
            let ob = Rect::from_center_size(pos2(close_r.left() - 62.0, head.center().y), vec2(104.0, 34.0));
            let over = pointer.map(|q| ob.contains(q)).unwrap_or(false);
            p.rect_filled(ob, CornerRadius::same(10), if over { pal.hover } else { pal.surface });
            p.text(ob.center(), Align2::CENTER_CENTER, t("Abrir con…"), FontId::proportional(13.5), pal.text);
            if over && clicked {
                action = Action::Open;
            }
        }
        if windowed.is_some() {
            // la cabecera propia arrastra la ventana; doble clic maximiza
            let over_head = pointer.map(|q| head.contains(q) && !close_r.contains(q)).unwrap_or(false);
            if over_head {
                if ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                    let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
                } else if ctx.input(|i| i.pointer.primary_pressed()) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
            }
        } else if let Some(q) = pointer {
            if clicked && !card.contains(q) {
                action = Action::Close;
            }
        }
        p.line_segment(
            [pos2(card.left() + 16.0, head.bottom()), pos2(card.right() - 16.0, head.bottom())],
            Stroke::new(1.0, pal.border),
        );

        // Pie con atajos
        let foot_h = 30.0;
        p.text(
            pos2(card.center().x, card.bottom() - foot_h / 2.0),
            Align2::CENTER_CENTER,
            if touch {
                t("Atrás o toque fuera: cerrar   ·   Pellizca: zoom   ·   Doble toque: ajustar")
            } else if self.kind == Kind::Video {
                t("Espacio: pausa / reanudar   ·   Left/Right: saltar 5 s   ·   Up/Down: anterior / siguiente   ·   O: abrir fuera   ·   Esc: cerrar")
            } else {
                t("Espacio / Esc: cerrar   ·   Flechas: anterior / siguiente   ·   Enter: abrir   ·   Rueda: zoom   ·   Doble clic: ajustar")
            },
            FontId::proportional(11.5),
            pal.subtle,
        );

        let content = Rect::from_min_max(
            pos2(card.left() + 14.0, head.bottom() + 12.0),
            pos2(card.right() - 14.0, card.bottom() - foot_h),
        );
        let cp = p.with_clip_rect(content);
        let in_content = pointer.map(|q| content.contains(q)).unwrap_or(false);

        // anterior / siguiente al tocar los bordes (táctil)
        if touch {
            let mid = content.center().y;
            for (dx, step, glyph) in [(content.left() + 26.0, -1isize, "‹"), (content.right() - 26.0, 1isize, "›")] {
                let r = Rect::from_center_size(pos2(dx, mid), vec2(46.0, 46.0));
                let over = pointer.map(|q| r.contains(q)).unwrap_or(false);
                p.circle_filled(r.center(), 23.0, Color32::from_black_alpha(if over { 170 } else { 110 }));
                p.text(r.center() + vec2(0.0, -2.0), Align2::CENTER_CENTER, glyph, FontId::proportional(30.0), Color32::WHITE);
                if over && clicked {
                    action = Action::Step(step);
                }
            }
        }

        let vt = self.player.as_ref().and_then(|p| p.tex.as_ref().map(|t| (t.id(), t.size_vec2())));
        if let Some((id, sz)) = vt {
            self.draw_zoomable(ctx, &cp, content, id, sz, in_content);
        } else {
        match &self.body {
            Body::Image(tex) => {
                let sz = tex.size_vec2();
                self.draw_zoomable(ctx, &cp, content, tex.id(), sz, in_content);
            }
            Body::Loading => {
                if let Some(t) = &self.thumb {
                    let sz = t.size_vec2();
                    let t = t.clone();
                    self.draw_zoomable(ctx, &cp, content, t.id(), sz, in_content);
                }
                if matches!(self.kind, Kind::Image | Kind::Video | Kind::Folder | Kind::Text | Kind::Archive) {
                    p.text(
                        pos2(content.center().x, content.bottom() - 14.0),
                        Align2::CENTER_CENTER,
                        t("Cargando…"),
                        FontId::proportional(12.0),
                        pal.subtle,
                    );
                }
            }
            Body::Text { lines, truncated } => {
                let lh = 18.0;
                let total = lines.len() as f32 * lh;
                let max_scroll = (total - content.height() + 20.0).max(0.0);
                if in_content {
                    let dy = ctx.input(|i| i.smooth_scroll_delta.y);
                    self.scroll = (self.scroll - dy).clamp(0.0, max_scroll);
                }
                let (pg, home, end) = ctx.input(|i| {
                    (
                        i.key_pressed(egui::Key::PageDown) as i32 - i.key_pressed(egui::Key::PageUp) as i32,
                        i.key_pressed(egui::Key::Home),
                        i.key_pressed(egui::Key::End),
                    )
                });
                self.scroll = (self.scroll + pg as f32 * content.height() * 0.9).clamp(0.0, max_scroll);
                if home {
                    self.scroll = 0.0;
                }
                if end {
                    self.scroll = max_scroll;
                }
                let bg = Rect::from_min_max(content.min, content.max);
                cp.rect_filled(bg, CornerRadius::same(10), pal.bg);
                let first = (self.scroll / lh) as usize;
                let n = (content.height() / lh) as usize + 2;
                let mono = FontId::monospace(13.0);
                let gutter = 54.0;
                for (k, line) in lines.iter().enumerate().skip(first).take(n) {
                    let y = content.top() + 10.0 + k as f32 * lh - self.scroll;
                    cp.text(pos2(content.left() + gutter - 10.0, y), Align2::RIGHT_TOP, k + 1, mono.clone(), pal.subtle.gamma_multiply(0.6));
                    cp.text(pos2(content.left() + gutter + 4.0, y), Align2::LEFT_TOP, line, mono.clone(), pal.text);
                }
                if *truncated {
                    cp.text(
                        pos2(content.right() - 12.0, content.bottom() - 8.0),
                        Align2::RIGHT_BOTTOM,
                        t("(vista parcial)"),
                        FontId::proportional(11.5),
                        pal.subtle,
                    );
                }
            }
            Body::Folder { count, names } => {
                let c = pos2(content.center().x, content.top() + 70.0);
                theme::draw_folder(&cp, Rect::from_center_size(c, vec2(96.0, 96.0)));
                cp.text(
                    pos2(content.center().x, c.y + 70.0),
                    Align2::CENTER_CENTER,
                    tf("{count} elementos", &[count]),
                    FontId::proportional(15.0),
                    pal.text,
                );
                let mut y = c.y + 104.0;
                for n in names.iter().take(((content.bottom() - y) / 20.0).max(0.0) as usize) {
                    cp.text(pos2(content.center().x, y), Align2::CENTER_CENTER, n, FontId::proportional(13.0), pal.subtle);
                    y += 20.0;
                }
            }
            Body::Archive { rows, truncated } => {
                let head = if *truncated { format!("{} {}", tf("{count} elementos", &[&rows.len()]), t("(índice parcial)")) } else { tf("{count} elementos", &[&rows.len()]) };
                cp.text(pos2(content.left() + 18.0, content.top() + 16.0), Align2::LEFT_CENTER, head, FontId::proportional(13.0), pal.text);
                let top = content.top() + 38.0;
                let fit = ((content.bottom() - top - 8.0) / 20.0).max(0.0) as usize;
                for (i, (n, sz, dir)) in rows.iter().take(fit).enumerate() {
                    let y = top + i as f32 * 20.0 + 10.0;
                    let maxw = content.width() - 36.0 - 90.0;
                    let mut name = n.clone();
                    let mut g = cp.layout_no_wrap(name.clone(), FontId::proportional(13.0), pal.text);
                    while g.size().x > maxw && name.chars().count() > 4 {
                        let keep = name.chars().count() - ((name.chars().count() / 8).max(1));
                        name = name.chars().take(keep).collect();
                        g = cp.layout_no_wrap(format!("{name}…"), FontId::proportional(13.0), pal.text);
                    }
                    let col = if *dir { pal.accent } else { pal.text };
                    cp.galley(pos2(content.left() + 18.0, y - g.size().y / 2.0), g, col);
                    if !*dir {
                        cp.text(pos2(content.right() - 18.0, y), Align2::RIGHT_CENTER, human_size(*sz), FontId::proportional(12.0), pal.subtle);
                    }
                }
                if rows.len() > fit {
                    cp.text(
                        pos2(content.center().x, content.bottom() - 12.0),
                        Align2::CENTER_CENTER,
                        format!("… +{}", rows.len() - fit),
                        FontId::proportional(12.0),
                        pal.subtle,
                    );
                }
            }
            Body::Info | Body::Error(_) => {
                let c = pos2(content.center().x, content.center().y - 30.0);
                let r = Rect::from_center_size(c, vec2(130.0, 130.0));
                let ext = self.name.rsplit_once('.').map(|x| x.1).unwrap_or("");
                theme::draw_file(&cp, r, self.kind, ext, pal);
                let msg = match &self.body {
                    Body::Error(e) => e.clone(),
                    _ => t("Sin vista previa disponible — pulsa Enter para abrir").to_string(),
                };
                cp.text(pos2(c.x, c.y + 86.0), Align2::CENTER_CENTER, msg, FontId::proportional(13.0), pal.subtle);
            }
        }
        }
        if self.kind == Kind::Video {
            self.video_controls(ctx, &p, content, pal, pointer, clicked);
        }
        action
    }

    fn video_controls(
        &mut self,
        ctx: &egui::Context,
        p: &egui::Painter,
        content: Rect,
        pal: &Pal,
        pointer: Option<Pos2>,
        clicked: bool,
    ) {
        if !crate::video::have_ffmpeg() {
            p.text(
                pos2(content.center().x, content.bottom() - 30.0),
                Align2::CENTER_CENTER,
                t("Para reproducir vídeo hace falta ffmpeg"),
                FontId::proportional(12.5),
                pal.subtle,
            );
            return;
        }
        let (playing, pos, dur, err) = match &self.player {
            Some(pl) => (pl.playing(), pl.position(), pl.duration, pl.error.clone()),
            None => (false, 0.0, 0.0, None),
        };
        let hit = |r: Rect| pointer.map(|q| r.contains(q)).unwrap_or(false) && clicked;
        let mut do_toggle = false;
        let mut do_seek: Option<f64> = None;
        // botón grande central mientras no hay reproducción iniciada
        if self.player.is_none() {
            let c = content.center();
            let r = Rect::from_center_size(c, vec2(84.0, 84.0));
            let hot = pointer.map(|q| r.contains(q)).unwrap_or(false);
            p.circle_filled(c, 42.0, Color32::from_black_alpha(if hot { 190 } else { 140 }));
            p.add(egui::Shape::convex_polygon(
                vec![c + vec2(-10.0, -18.0), c + vec2(-10.0, 18.0), c + vec2(18.0, 0.0)],
                Color32::WHITE,
                Stroke::NONE,
            ));
            if hit(r) {
                do_toggle = true;
            }
        }
        let bar = Rect::from_min_size(
            pos2(content.left() + 16.0, content.bottom() - 52.0),
            vec2(content.width() - 32.0, 38.0),
        );
        p.rect_filled(bar, CornerRadius::same(14), Color32::from_black_alpha(160));
        let bt = Rect::from_center_size(pos2(bar.left() + 26.0, bar.center().y), vec2(34.0, 34.0));
        let c = bt.center();
        if playing {
            for dx in [-4.5, 4.5] {
                p.rect_filled(Rect::from_center_size(c + vec2(dx, 0.0), vec2(5.0, 16.0)), 1.5, Color32::WHITE);
            }
        } else {
            p.add(egui::Shape::convex_polygon(
                vec![c + vec2(-5.0, -9.0), c + vec2(-5.0, 9.0), c + vec2(9.0, 0.0)],
                Color32::WHITE,
                Stroke::NONE,
            ));
        }
        if hit(bt) {
            do_toggle = true;
        }
        let label = format!("{} / {}", fmt_secs(pos), if dur > 0.0 { fmt_secs(dur) } else { "--:--".into() });
        let tw = p.text(
            pos2(bt.right() + 10.0, bar.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::monospace(12.5),
            Color32::WHITE,
        );
        let sb = Rect::from_min_max(pos2(tw.right() + 16.0, bar.center().y - 3.0), pos2(bar.right() - 18.0, bar.center().y + 3.0));
        if sb.width() > 20.0 {
            p.rect_filled(sb, 3.0, Color32::from_white_alpha(60));
            let f = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
            let fill = Rect::from_min_size(sb.min, vec2(sb.width() * f, sb.height()));
            p.rect_filled(fill, 3.0, pal.accent);
            p.circle_filled(pos2(sb.left() + sb.width() * f, sb.center().y), 6.0, Color32::WHITE);
            let hit_r = sb.expand2(vec2(8.0, 12.0));
            if hit(hit_r) && dur > 0.0 {
                if let Some(q) = pointer {
                    do_seek = Some(((q.x - sb.left()) / sb.width()).clamp(0.0, 1.0) as f64 * dur);
                }
            }
        }
        if let Some(e) = err {
            p.text(
                pos2(content.center().x, bar.top() - 14.0),
                Align2::CENTER_CENTER,
                tf("{}: {}", &[&t("No se pudo reproducir el vídeo"), &e]),
                FontId::proportional(12.5),
                Color32::from_rgb(235, 100, 100),
            );
        }
        if do_toggle {
            self.toggle_play(ctx);
        } else if let Some(s) = do_seek {
            let path = self.path.clone();
            let pl = self.player.get_or_insert_with(|| crate::video::Player::new(ctx, &path));
            pl.seek_to(s);
        }
    }

    fn draw_zoomable(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, tex: egui::TextureId, sz: egui::Vec2, hot: bool) {
        if hot {
            let pinch = ctx.input(|i| i.zoom_delta());
            if pinch != 1.0 {
                self.zoom = (self.zoom * pinch).clamp(0.25, 24.0);
            }
            let (dy, drag, dbl) = ctx.input(|i| {
                (
                    i.smooth_scroll_delta.y,
                    if i.pointer.primary_down() { i.pointer.delta() } else { vec2(0.0, 0.0) },
                    i.pointer.button_double_clicked(egui::PointerButton::Primary),
                )
            });
            if dy != 0.0 {
                self.zoom = (self.zoom * (dy * 0.0015).exp()).clamp(0.25, 24.0);
            }
            self.pan += drag;
            if dbl {
                self.zoom = 1.0;
                self.pan = vec2(0.0, 0.0);
            }
        }
        let avail = area.shrink(8.0);
        let fit = (avail.width() / sz.x).min(avail.height() / sz.y).min(f32::MAX);
        // Las miniaturas pequeñas se amplían hasta llenar; las imágenes grandes se ajustan.
        let scale = fit * self.zoom;
        let size = sz * scale;
        // la imagen no puede salirse del todo: siempre queda una franja visible
        let lim = vec2(((size.x + area.width()) / 2.0 - 60.0).max(0.0), ((size.y + area.height()) / 2.0 - 60.0).max(0.0));
        self.pan = vec2(self.pan.x.clamp(-lim.x, lim.x), self.pan.y.clamp(-lim.y, lim.y));
        let rect = Rect::from_center_size(area.center() + self.pan, size);
        let shape = egui::epaint::RectShape::filled(rect, CornerRadius::same(8), Color32::WHITE)
            .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
        p.add(shape);
        let _: Option<Pos2> = None;
    }
}
