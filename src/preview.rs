//! Vista rápida (Espacio): imágenes a pantalla casi completa con zoom/pan, texto virtualizado,
//! carpetas y ficha de información. Un único hilo de fondo; sólo se atiende la última petición.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::fsys::{fmt_time, human_size, Kind};
use crate::i18n::{t, tf};
use crate::theme::{self, Glyph, Pal};
use crate::thumbs::{decode_image, lock, open_regular, Decoded};
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
    Text(TextData),
    Folder(usize, Vec<String>),
    Archive(crate::archive::Listing),
    Binary,
    Mesh(crate::mesh3d::Mesh),
    /// documento de oficina, PDF o cómic (el estado vive en `Preview::doc`)
    Doc,
    Rich(crate::rich::Rich),
    Tables(Vec<(String, crate::table::Table)>),
    Tree(crate::tree::Tree, TextData),
    Err(String),
}

/// Texto plano ya leído: líneas, si falta algo y dónde está el hueco entre el principio y el final.
pub struct TextData {
    lines: Vec<String>,
    truncated: bool,
    /// (índice de la línea que marca el hueco, número real —desde 0— de la línea que le sigue, si se sabe)
    gap: Option<(usize, Option<usize>)>,
}

impl TextData {
    /// Número de línea (desde 1) de la línea `k`, si se sabe.
    fn number(&self, k: usize) -> Option<usize> {
        match self.gap {
            Some((g, _)) if k == g => None,
            Some((g, base)) if k > g => base.map(|b| b + (k - g - 1) + 1),
            _ => Some(k + 1),
        }
    }

    /// Total de líneas del archivo, si se sabe.
    fn total(&self) -> Option<usize> {
        match self.gap {
            None => Some(self.lines.len()),
            Some((g, Some(b))) => Some(b + self.lines.len().saturating_sub(g + 1)),
            Some((_, None)) => None,
        }
    }
}

pub enum Body {
    Loading,
    Image(egui::TextureHandle),
    Text(TextData),
    Folder { count: usize, names: Vec<String> },
    Archive { rows: Vec<(String, u64, bool)>, truncated: bool },
    Info,
    /// modelo 3D (el estado vive en `Preview::mesh`)
    Mesh,
    /// documento paginado (el estado vive en `Preview::doc`)
    Doc,
    /// Markdown o libro (el estado vive en `Preview::rich`)
    Rich,
    /// CSV o base de datos (el estado vive en `Preview::table`)
    Table,
    /// JSON o XML: árbol y, en la otra pestaña, el texto
    Tree(TextData),
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
    pub audio: Option<crate::audio::AudioPlayer>,
    mesh: Option<MeshView>,
    doc: Option<crate::docview::DocView>,
    rich: Option<crate::rich::RichView>,
    table: Option<crate::table::TableView>,
    tree: Option<crate::tree::TreeView>,
    /// pestaña «Texto» del árbol JSON/XML
    tree_text: bool,
    /// datos del archivo (panel «i») y su carga en segundo plano
    pub meta: Vec<(String, String)>,
    meta_rx: Option<Receiver<Vec<(String, String)>>>,
    meta_started: bool,
    show_meta: bool,
    /// archivo abierto dentro de un comprimido (vista anidada) y su carpeta temporal
    inner: Option<Box<(Preview, Worker)>>,
    entry_pick: Option<String>,
    tmp: Option<PathBuf>,
    entry_rx: Option<Receiver<Result<PathBuf, String>>>,
    entry_busy: Option<String>,
    entry_err: Option<String>,
    /// búsqueda y selección de texto en documentos
    find_open: bool,
    find_text: String,
    sel: Option<DocSel>,
    page_runs: Option<(usize, Vec<crate::docview::Run>)>,
    doc_rect: Option<Rect>,
    sel_drag: bool,
    zoom: f32,
    pan: egui::Vec2,
    /// cuánto puede moverse la página ampliada (lo calcula el último dibujo)
    pan_lim: egui::Vec2,
    /// rueda acumulada para cambiar de página
    scroll: f32,
    /// texto plano: posición, ajuste, búsqueda y selección
    txt: TextView,
    /// desplazamiento pendiente pedido con las flechas (vista rápida)
    nudge: f32,
    /// ¿se dibuja como vista rápida (no en el panel lateral)?
    quick: bool,
    /// llevar a la vista la coincidencia actual del documento
    hit_jump: bool,
    /// página recordada que se abre cuando el documento esté listo
    want_page: Option<usize>,
    /// el documento se abre cuando la selección lleva un rato quieta
    opened_at: web_time::Instant,
    doc_pending: bool,
    /// Última posición conocida del puntero (al soltar un dedo ya no hay posición).
    last_pos: Option<Pos2>,
    /// pestaña «Uso de disco» de las carpetas
    show_usage: bool,
    usage: Option<UsageView>,
}

/// Selección de texto en una página: (trozo, carácter) de inicio y de fin.
#[derive(Clone, Copy)]
struct DocSel {
    a: (usize, usize),
    b: (usize, usize),
    page: usize,
}

/// Modelo 3D abierto: cámara y última imagen dibujada.
struct MeshView {
    mesh: Arc<crate::mesh3d::Mesh>,
    yaw: f32,
    pitch: f32,
    zoom: f32,
    tex: Option<egui::TextureHandle>,
    /// parámetros de la última imagen: (ancho, alto, guiñada, inclinación, zoom, oscuro, completa)
    key: Option<(usize, usize, i32, i32, i32, bool, bool)>,
    dragging: bool,
    /// imagen en preparación en otro hilo (el visor no se bloquea al dibujar)
    job: Option<(MeshKey, std::sync::mpsc::Receiver<Vec<u8>>)>,
    /// dibujo por GPU (si falla se usa el software)
    gpu: crate::gpu3d::Shared,
}

type MeshKey = (usize, usize, i32, i32, i32, bool, bool);

/// Gráfico de uso de disco de una carpeta: análisis en curso, árbol resultante y nivel mostrado.
pub struct UsageView {
    scan: crate::diskusage::Scan,
    root: Option<Arc<crate::diskusage::Node>>,
    path: Vec<usize>,
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
    /// al soltar el `Worker` el hilo despierta y termina
    quit: Arc<std::sync::atomic::AtomicBool>,
    rx: Receiver<(u64, Raw)>,
    generation: u64,
    /// sin hilos (navegador): el trabajo se hace al pedirlo
    #[cfg(target_arch = "wasm32")]
    tx: std::sync::mpsc::Sender<(u64, Raw)>,
}

impl Worker {
    pub fn new(ctx: &egui::Context) -> Worker {
        let slot: Arc<(Mutex<Option<Job>>, Condvar)> = Arc::new((Mutex::new(None), Condvar::new()));
        let (tx, rx) = channel();
        let quit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, ctx, q) = (slot.clone(), ctx.clone(), quit.clone());
        #[cfg(target_arch = "wasm32")]
        let _ = (&s, &ctx, &q);
        #[cfg(not(target_arch = "wasm32"))]
        let tx2 = tx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        crate::thumbs::spawn_decoder("vista previa", move || loop {
            let job = {
                let mut g = lock(&s.0);
                loop {
                    if q.load(std::sync::atomic::Ordering::Acquire) {
                        return;
                    }
                    if let Some(j) = g.take() {
                        break j;
                    }
                    g = s.1.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            };
            let raw = load_guarded(&job);
            if tx2.send((job.generation, raw)).is_err() {
                return;
            }
            ctx.request_repaint();
        });
        Worker { slot, quit, rx, generation: 0, #[cfg(target_arch = "wasm32")] tx }
    }

    fn submit(&mut self, path: PathBuf, kind: Kind) -> u64 {
        self.generation += 1;
        let job = Job { generation: self.generation, path, kind };
        #[cfg(target_arch = "wasm32")]
        {
            let _ = self.tx.send((job.generation, load_guarded(&job)));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            *lock(&self.slot.0) = Some(job);
            self.slot.1.notify_one();
        }
        self.generation
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // despierta al hilo para que vea la orden de salir
        self.quit.store(true, std::sync::atomic::Ordering::Release);
        let _g = lock(&self.slot.0);
        self.slot.1.notify_one();
    }
}

fn fmt_secs(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Alto de una fila de texto plano.
const LH: f32 = 18.0;
/// Coincidencias como mucho en una búsqueda dentro del texto.
const TEXT_HITS: usize = 5000;

/// Visor de texto plano: posición, ajuste de líneas, búsqueda y selección.
#[derive(Default)]
struct TextView {
    /// desplazamiento vertical (px) y horizontal (px, sin ajuste)
    scroll: f32,
    scroll_x: f32,
    wrap: bool,
    /// caracteres de cada línea y la más larga (se calculan una vez)
    lens: Vec<u32>,
    max_len: usize,
    /// fila visual en que empieza cada línea (+ total al final) y columnas con que se calculó (0 = sin ajuste)
    starts: Vec<u32>,
    starts_cols: usize,
    /// primera línea visible (con fracción): la posición que se conserva al cambiar ajuste o ancho
    top: f32,
    /// coincidencias (línea, carácter), longitud de la buscada y actual
    hits: Vec<(usize, usize)>,
    hit_len: usize,
    cur: usize,
    jump: bool,
    /// selección con el ratón: ancla y extremo (línea, carácter)
    sel: Option<((usize, usize), (usize, usize))>,
    sel_drag: bool,
}

impl TextView {
    /// Visor que empieza en la línea `top`.
    fn at(top: f32) -> TextView {
        TextView { top, starts_cols: usize::MAX, ..Default::default() }
    }

    fn search(&mut self, td: &TextData, q: &str) {
        self.hits.clear();
        self.cur = 0;
        let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
        let qc: Vec<char> = q.chars().map(lower).collect();
        self.hit_len = qc.len();
        if qc.is_empty() {
            return;
        }
        'all: for (k, l) in td.lines.iter().enumerate() {
            if td.gap.is_some_and(|g| g.0 == k) {
                continue;
            }
            let lc: Vec<char> = l.chars().map(lower).collect();
            let mut i = 0;
            while i + qc.len() <= lc.len() {
                if lc[i..i + qc.len()] == qc[..] {
                    self.hits.push((k, i));
                    if self.hits.len() >= TEXT_HITS {
                        break 'all;
                    }
                    i += qc.len();
                } else {
                    i += 1;
                }
            }
        }
        // la primera a partir de lo que se está viendo
        let top = self.top.max(0.0) as usize;
        self.cur = self.hits.iter().position(|h| h.0 >= top).unwrap_or(0);
        self.jump = true;
    }

    fn step_hit(&mut self, d: isize) {
        if !self.hits.is_empty() {
            self.cur = (self.cur as isize + d).rem_euclid(self.hits.len() as isize) as usize;
            self.jump = true;
        }
    }
}

/// Trozo `[a, b)` (en caracteres) de una línea.
fn char_slice(l: &str, a: usize, b: usize) -> &str {
    let mut it = l.char_indices().map(|x| x.0).chain(std::iter::once(l.len()));
    let s = it.by_ref().nth(a).unwrap_or(l.len());
    let e = if b > a { it.nth(b - a - 1).unwrap_or(l.len()) } else { s };
    &l[s..e]
}

/// Texto plano virtualizado: números de línea, rueda en los dos ejes, arrastre táctil, barras que se
/// pueden arrastrar, ajuste de líneas, búsqueda resaltada y selección con copia.
#[allow(clippy::too_many_arguments)]
fn draw_text(ctx: &egui::Context, p: &egui::Painter, area: Rect, hot: bool, active: bool, keys: bool, copy_all: bool, tv: &mut TextView, td: &TextData, nudge: f32, click: Option<Pos2>, find: Option<Rect>, pal: &Pal) {
    let lines = &td.lines;
    let n = lines.len();
    let mono = FontId::monospace(13.0);
    let cw = (p.layout_no_wrap("0000000000".into(), mono.clone(), pal.text).size().x / 10.0).max(1.0);
    p.rect_filled(area, CornerRadius::same(10), pal.bg);
    let bar_h = 24.0;
    let view = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - bar_h));
    let digits = td.total().unwrap_or(n).max(1).to_string().len().max(3) as f32;
    let gutter = digits * cw + 18.0;
    let tx0 = view.left() + gutter + 6.0;
    let text_w = (view.right() - 16.0 - tx0).max(cw * 8.0);
    let cols = ((text_w / cw).floor() as usize).max(8);
    if tv.lens.len() != n {
        tv.lens = lines.iter().map(|l| l.chars().count() as u32).collect();
        tv.max_len = tv.lens.iter().copied().max().unwrap_or(0) as usize;
        tv.starts_cols = usize::MAX;
    }
    let wc = if tv.wrap { cols } else { 0 };
    let relayout = tv.starts_cols != wc;
    if relayout {
        tv.starts.clear();
        let mut acc = 0u32;
        for &k in &tv.lens {
            tv.starts.push(acc);
            acc += if wc == 0 { 1 } else { (k as usize).div_ceil(wc).max(1) as u32 };
        }
        tv.starts.push(acc);
        tv.starts_cols = wc;
    }
    let starts = std::mem::take(&mut tv.starts);
    let rows = *starts.last().unwrap_or(&0) as usize;
    let line_of = |r: usize| -> usize { starts.partition_point(|&s| s as usize <= r).saturating_sub(1).min(n.saturating_sub(1)) };
    let row_of = |l: usize, c: usize| -> usize {
        let l = l.min(n.saturating_sub(1));
        let span = (starts[l + 1] - starts[l]).max(1) as usize;
        starts[l] as usize + if wc == 0 { 0 } else { (c / wc).min(span - 1) }
    };
    // tras recalcular las filas (ajuste, ancho o posición recordada) se vuelve a la misma línea
    if relayout && n > 0 {
        let l = (tv.top.max(0.0).floor() as usize).min(n - 1);
        let frac = (tv.top - l as f32).clamp(0.0, 1.0);
        tv.scroll = (starts[l] as f32 + frac * (starts[l + 1] - starts[l]) as f32) * LH;
    }
    let total = rows as f32 * LH + 20.0;
    let max_y = (total - view.height()).max(0.0);
    let total_w = if tv.wrap { 0.0 } else { tv.max_len as f32 * cw + 24.0 };
    let max_x = (total_w - text_w).max(0.0);

    // entrada: flechas, rueda (los dos ejes), dedo, teclas de página
    tv.scroll += nudge;
    let touch = cfg!(target_os = "android") || ctx.input(|i| i.any_touches());
    let (d, pd, down, pressed, origin, pos, edge) = ctx.input(|i| {
        (i.smooth_scroll_delta, i.pointer.delta(), i.pointer.primary_down(), i.pointer.primary_pressed(), i.pointer.press_origin(), i.pointer.latest_pos(), edge_start(i))
    });
    let vtrack = Rect::from_min_max(pos2(view.right() - 13.0, view.top() + 4.0), pos2(view.right() - 1.0, view.bottom() - 4.0));
    let htrack = Rect::from_min_max(pos2(tx0, view.bottom() - 11.0), pos2(view.right() - 16.0, view.bottom() - 1.0));
    let on_bars = |q: Pos2| (max_y > 0.0 && vtrack.expand(3.0).contains(q)) || (max_x > 0.0 && htrack.expand(3.0).contains(q));
    if hot {
        tv.scroll -= d.y;
        tv.scroll_x -= d.x;
        if touch && down && !edge && origin.is_some_and(|o| view.contains(o) && !on_bars(o)) {
            tv.scroll -= pd.y;
            tv.scroll_x -= pd.x;
        }
    }
    if keys {
        let (pg, home, end) = ctx.input(|i| (i.key_pressed(egui::Key::PageDown) as i32 - i.key_pressed(egui::Key::PageUp) as i32, i.key_pressed(egui::Key::Home), i.key_pressed(egui::Key::End)));
        tv.scroll += pg as f32 * view.height() * 0.9;
        if home {
            tv.scroll = 0.0;
            tv.scroll_x = 0.0;
        }
        if end {
            tv.scroll = max_y;
        }
    }
    if tv.jump && n > 0 {
        tv.jump = false;
        if let Some(&(l, c)) = tv.hits.get(tv.cur) {
            let y = row_of(l, c) as f32 * LH;
            // por debajo del cuadro de búsqueda
            if y < tv.scroll + 50.0 || y > tv.scroll + view.height() - 2.0 * LH {
                tv.scroll = y - view.height() / 3.0;
            }
            if !tv.wrap {
                let (x0, x1) = (c as f32 * cw, (c + tv.hit_len) as f32 * cw);
                if x0 < tv.scroll_x || x1 > tv.scroll_x + text_w {
                    tv.scroll_x = x0 - text_w / 3.0;
                }
            }
        }
    }
    // selección con el ratón (con el dedo, arrastrar desplaza)
    let hit_at = |q: Pos2, scroll: f32, scroll_x: f32| -> (usize, usize) {
        let r = (((q.y - view.top() - 10.0 + scroll) / LH).floor().max(0.0) as usize).min(rows.saturating_sub(1));
        let l = line_of(r);
        let len = tv.lens.get(l).copied().unwrap_or(0) as usize;
        let x = ((q.x - tx0 + if wc == 0 { scroll_x } else { 0.0 }) / cw).round().max(0.0) as usize;
        let c = if wc == 0 { x } else { (r - starts[l] as usize) * wc + x.min(wc) };
        (l, c.min(len))
    };
    if n > 0 && !touch && hot && pressed {
        if let Some(o) = origin.filter(|o| view.contains(*o) && !on_bars(*o) && !find.is_some_and(|f| f.contains(*o))) {
            let a = hit_at(o, tv.scroll, tv.scroll_x);
            tv.sel = Some((a, a));
            tv.sel_drag = true;
        }
    }
    if tv.sel_drag {
        if down && n > 0 {
            if let Some(q) = pos {
                // fuera del área: desplaza para seguir seleccionando
                if q.y < view.top() {
                    tv.scroll -= LH;
                } else if q.y > view.bottom() {
                    tv.scroll += LH;
                }
                if wc == 0 && q.x > view.right() - 16.0 {
                    tv.scroll_x += cw * 2.0;
                } else if wc == 0 && q.x < tx0 && tv.scroll_x > 0.0 {
                    tv.scroll_x -= cw * 2.0;
                }
                tv.scroll = tv.scroll.clamp(0.0, max_y);
                tv.scroll_x = tv.scroll_x.clamp(0.0, max_x);
                let b = hit_at(q, tv.scroll, tv.scroll_x);
                if let Some(s) = &mut tv.sel {
                    s.1 = b;
                }
                if !view.contains(q) {
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                }
            }
        } else {
            tv.sel_drag = false;
        }
    }
    tv.scroll = tv.scroll.clamp(0.0, max_y);
    tv.scroll_x = tv.scroll_x.clamp(0.0, max_x);
    let sel = tv.sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) }).filter(|(a, b)| a != b);
    // copiar: la selección; en la vista rápida, sin selección, lo que se ve
    if keys && n > 0 && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
        let range = sel.or_else(|| {
            copy_all.then(|| {
                let a = line_of((tv.scroll / LH) as usize);
                let b = line_of(((tv.scroll + view.height()) / LH) as usize);
                ((a, 0), (b, tv.lens.get(b).copied().unwrap_or(0) as usize))
            })
        });
        if let Some((a, b)) = range {
            let mut out = String::new();
            for l in a.0..=b.0.min(n - 1) {
                if td.gap.is_some_and(|g| g.0 == l) {
                    continue;
                }
                let c0 = if l == a.0 { a.1 } else { 0 };
                let c1 = if l == b.0 { b.1 } else { usize::MAX };
                if l > a.0 {
                    out.push('\n');
                }
                out.push_str(char_slice(&lines[l], c0, c1.min(tv.lens[l] as usize)));
            }
            ctx.copy_text(out);
        }
    }

    // dibujo de las filas visibles
    let first = (tv.scroll / LH).floor() as usize;
    let nvis = (view.height() / LH) as usize + 2;
    let cx0 = if wc == 0 { (tv.scroll_x / cw).floor() as usize } else { 0 };
    let xoff = if wc == 0 { tv.scroll_x - cx0 as f32 * cw } else { 0.0 };
    let vis_cols = (text_w / cw) as usize + 3;
    let gp = p.with_clip_rect(Rect::from_min_max(view.min, pos2(tx0 - 2.0, view.bottom())).intersect(p.clip_rect()));
    let tp = p.with_clip_rect(Rect::from_min_max(pos2(tx0 - 2.0, view.top()), view.max).intersect(p.clip_rect()));
    let sel_col = Color32::from_rgba_unmultiplied(60, 120, 255, 90);
    for r in first..(first + nvis).min(rows) {
        let l = line_of(r);
        let sub = r - starts[l] as usize;
        let y = view.top() + 10.0 + r as f32 * LH - tv.scroll;
        if td.gap.is_some_and(|g| g.0 == l) {
            tp.line_segment([pos2(tx0, y + LH / 2.0), pos2(view.right() - 16.0, y + LH / 2.0)], Stroke::new(1.0, pal.border));
            let msg = t("… parte central del archivo omitida …");
            let g = p.layout_no_wrap(msg.into(), FontId::proportional(12.0), pal.subtle);
            let r = Rect::from_center_size(pos2(view.center().x, y + LH / 2.0), g.size() + vec2(16.0, 2.0));
            p.rect_filled(r, CornerRadius::same(6), pal.bg);
            p.galley(r.min + vec2(8.0, 1.0), g, pal.subtle);
            continue;
        }
        if sub == 0 {
            if let Some(num) = td.number(l) {
                gp.text(pos2(tx0 - 16.0, y), Align2::RIGHT_TOP, num, mono.clone(), pal.subtle.gamma_multiply(0.6));
            }
        } else {
            gp.text(pos2(tx0 - 16.0, y), Align2::RIGHT_TOP, "↪", mono.clone(), pal.subtle.gamma_multiply(0.4));
        }
        let len = tv.lens[l] as usize;
        let (a, b) = if wc == 0 { (cx0, (cx0 + vis_cols).min(len)) } else { (sub * wc, ((sub + 1) * wc).min(len)) };
        let x_of = |c: usize| tx0 - xoff + (c as f32 - a as f32) * cw;
        let last_row = wc == 0 || (sub + 1) * wc >= len;
        if let Some((s, e)) = sel.filter(|(s, e)| l >= s.0 && l <= e.0) {
            let c0 = (if l == s.0 { s.1 } else { 0 }).max(a);
            // el salto de línea seleccionado se ve como un trocito más
            let c1 = (if l == e.0 { e.1 } else { len + 1 }).min(if last_row { b.max(len) + 1 } else { b });
            if c1 > c0 {
                tp.rect_filled(Rect::from_min_max(pos2(x_of(c0), y), pos2(x_of(c1), y + LH)), CornerRadius::same(2), sel_col);
            }
        }
        if !tv.hits.is_empty() {
            let from = tv.hits.partition_point(|h| h.0 < l);
            for (k, &(hl, hc)) in tv.hits.iter().enumerate().skip(from) {
                if hl != l {
                    break;
                }
                let (h0, h1) = (hc.max(a), (hc + tv.hit_len).min(b));
                if h1 > h0 {
                    let col = if k == tv.cur { Color32::from_rgba_unmultiplied(255, 140, 0, 140) } else { Color32::from_rgba_unmultiplied(255, 220, 0, 100) };
                    tp.rect_filled(Rect::from_min_max(pos2(x_of(h0), y), pos2(x_of(h1), y + LH)), CornerRadius::same(2), col);
                }
            }
        }
        if b > a {
            tp.text(pos2(tx0 - xoff, y), Align2::LEFT_TOP, char_slice(&lines[l], a, b), mono.clone(), pal.text);
        }
    }
    if n > 0 && !touch && hot && !tv.sel_drag && pos.is_some_and(|q| view.contains(q) && q.x >= tx0 - 4.0 && !on_bars(q) && !find.is_some_and(|f| f.contains(q))) {
        ctx.set_cursor_icon(egui::CursorIcon::Text);
    }
    // barras (se pueden pulsar y arrastrar)
    crate::rich::scrollbar(ctx, p, view, total, &mut tv.scroll, pal, active);
    if max_x > 0.0 {
        crate::rich::scrollbar_track(ctx, p, htrack, text_w, total_w, &mut tv.scroll_x, pal, active, false);
    }
    // posición actual
    tv.top = if n == 0 {
        0.0
    } else {
        let r = tv.scroll / LH;
        let l = line_of(r as usize);
        let span = (starts[l + 1] - starts[l]).max(1) as f32;
        l as f32 + ((r - starts[l] as f32) / span).clamp(0.0, 0.999)
    };
    tv.starts = starts;

    // pie: línea, aviso de vista parcial y botón de ajuste
    let bar = Rect::from_min_max(pos2(area.left(), area.bottom() - bar_h), area.max);
    p.line_segment([pos2(bar.left() + 8.0, bar.top()), pos2(bar.right() - 8.0, bar.top())], Stroke::new(1.0, pal.border.gamma_multiply(0.6)));
    let label = t("Ajustar líneas");
    let g = p.layout_no_wrap(label.into(), FontId::proportional(11.5), if tv.wrap { pal.accent } else { pal.text });
    let br = Rect::from_min_size(pos2(bar.right() - g.size().x - 22.0, bar.center().y - 10.0), vec2(g.size().x + 16.0, 20.0));
    let over = active && pos.is_some_and(|q| br.contains(q));
    if tv.wrap || over {
        p.rect_filled(br, CornerRadius::same(6), if tv.wrap { pal.sel } else { pal.hover });
    }
    p.galley(pos2(br.left() + 8.0, br.center().y - g.size().y / 2.0), g, pal.text);
    if click.is_some_and(|q| br.contains(q)) {
        tv.wrap = !tv.wrap;
        tv.starts_cols = usize::MAX;
        tv.scroll_x = 0.0;
        ctx.request_repaint();
    }
    let top_line = (tv.top as usize).min(n.saturating_sub(1));
    let mut info = match (td.number(top_line), td.total()) {
        _ if n == 0 => String::new(),
        (Some(k), Some(all)) => tf("Línea {} de {}", &[&k, &all]),
        (Some(k), None) => tf("Línea {}", &[&k]),
        (None, _) => t("Final del archivo").to_string(),
    };
    if td.gap.is_some() {
        info = format!("{info}   ·   {}", t("vista parcial: principio y final del archivo"));
    } else if td.truncated {
        info = format!("{info}   ·   {}", t("(vista parcial)"));
    }
    p.with_clip_rect(Rect::from_min_max(bar.min, pos2(br.left() - 6.0, bar.bottom())).intersect(p.clip_rect()))
        .text(pos2(bar.left() + 12.0, bar.center().y), Align2::LEFT_CENTER, info, FontId::proportional(11.5), pal.subtle);
}

fn read_limited(path: &std::path::Path, max: u64) -> Option<String> {
    // sólo archivos normales: un FIFO o un dispositivo dejaría colgado el hilo
    let f = open_regular(path)?;
    if f.metadata().ok()?.len() > max {
        return None;
    }
    let mut b = Vec::new();
    f.take(max).read_to_end(&mut b).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

/// Bytes que se leen del principio de un archivo de texto y, si es mayor, del final.
const TEXT_HEAD: u64 = 4 << 20;
const TEXT_TAIL: u64 = 1 << 20;
/// Líneas como mucho del principio y del final.
const TEXT_LINES: usize = 100_000;
const TAIL_LINES: usize = 20_000;
/// Una línea más larga se corta (texto minimizado de una sola línea).
const LINE_CHARS: usize = 20_000;

/// Decodifica UTF-8 (con o sin BOM) o UTF-16 (con BOM: `Some(true)` = little endian).
fn decode_text(b: &[u8], utf16: Option<bool>) -> String {
    match utf16 {
        Some(le) => {
            let units = b.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
            char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
        }
        None => String::from_utf8_lossy(b).into_owned(),
    }
}

fn push_line(out: &mut Vec<String>, l: &str, truncated: &mut bool) {
    let l = l.replace('\t', "    ");
    if l.len() > LINE_CHARS && l.chars().count() > LINE_CHARS {
        *truncated = true;
        out.push(l.chars().take(LINE_CHARS).collect::<String>() + "…");
    } else {
        out.push(l);
    }
}

/// Texto de un archivo: el principio y, si es grande, también el final (para llegar al final de un registro).
fn text_raw(path: &std::path::Path) -> Raw {
    use std::io::{Seek, SeekFrom};
    // una tubería con nombre o un dispositivo bloquearían para siempre el hilo de vistas previas
    let Some(mut f) = open_regular(path) else {
        return Raw::Err(t("No se pudo abrir").into());
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    let _ = Read::by_ref(&mut f).take(TEXT_HEAD).read_to_end(&mut buf);
    // UTF-16 con BOM (Windows) también es texto aunque tenga ceros
    let (utf16, bom) = if buf.starts_with(&[0xFF, 0xFE]) {
        (Some(true), 2)
    } else if buf.starts_with(&[0xFE, 0xFF]) {
        (Some(false), 2)
    } else if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (None, 3)
    } else {
        (None, 0)
    };
    if utf16.is_none() && buf[..buf.len().min(4096)].contains(&0) {
        return Raw::Binary;
    }
    let whole = buf.len() as u64 >= len;
    let txt = decode_text(&buf[bom..], utf16);
    let mut truncated = !whole;
    let mut lines: Vec<String> = Vec::new();
    let mut gap = None;
    let n_all = if whole { txt.lines().count() } else { usize::MAX };
    if whole && n_all > TEXT_LINES {
        // cabe entero pero tiene demasiadas líneas: principio y final, con los números de verdad
        truncated = true;
        let (head, tail_from) = (TEXT_LINES - TAIL_LINES, n_all - TAIL_LINES);
        for (i, l) in txt.lines().enumerate() {
            if i == tail_from {
                gap = Some((lines.len(), Some(tail_from)));
                lines.push(String::new());
            }
            if i < head || i >= tail_from {
                push_line(&mut lines, l, &mut truncated);
            }
        }
    } else {
        for l in txt.lines() {
            if lines.len() >= TEXT_LINES {
                truncated = true;
                break;
            }
            push_line(&mut lines, l, &mut truncated);
        }
    }
    if !whole && len > TEXT_HEAD {
        // archivo grande: la última línea leída puede estar cortada
        if !txt.ends_with('\n') && lines.len() > 1 {
            lines.pop();
        }
        let mut start = len.saturating_sub(TEXT_TAIL).max(buf.len() as u64);
        if utf16.is_some() && start % 2 == 1 {
            start += 1;
        }
        let mut tail = Vec::new();
        if f.seek(SeekFrom::Start(start)).is_ok() && Read::by_ref(&mut f).take(TEXT_TAIL).read_to_end(&mut tail).is_ok() && !tail.is_empty() {
            let tt = decode_text(&tail, utf16);
            // la primera línea del trozo final casi siempre empieza a medias
            let tt = tt.split_once('\n').map(|x| x.1).unwrap_or("");
            let rest: Vec<&str> = tt.lines().collect();
            let skip = rest.len().saturating_sub(TAIL_LINES);
            let g = lines.len();
            lines.push(String::new());
            gap = Some((g, None));
            for l in &rest[skip..] {
                push_line(&mut lines, l, &mut truncated);
            }
        }
    }
    Raw::Text(TextData { lines, truncated, gap })
}

/// `load` con los pánicos atrapados: un archivo raro sólo estropea su propia vista previa.
fn load_guarded(job: &Job) -> Raw {
    crate::thumbs::guarded(|| load(job)).unwrap_or_else(|| Raw::Err(t("No se pudo mostrar el documento").into()))
}

fn load(job: &Job) -> Raw {
    let remote = crate::sftp::is_remote(&job.path);
    // un FIFO, un socket o un dispositivo no se leen nunca (se quedaría bloqueado el hilo)
    if job.kind != Kind::Folder && !remote && crate::vfs::fs::metadata(&job.path).is_ok_and(|m| !m.v_is_file() && !m.v_is_dir()) {
        return Raw::Binary;
    }
    if job.kind != Kind::Folder && !remote {
        if crate::docview::is_paged_path(&job.path) {
            return Raw::Doc;
        }
        let ext = job.path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "epub" => {
                return match crate::rich::epub(&job.path) {
                    Ok(r) => Raw::Rich(r),
                    Err(e) => Raw::Err(e),
                }
            }
            "md" | "markdown" | "mdown" => {
                if let Some(txt) = read_limited(&job.path, 4 << 20) {
                    return Raw::Rich(crate::rich::markdown(&txt));
                }
            }
            "csv" | "tsv" => {
                if let Some(txt) = read_limited(&job.path, 24 << 20) {
                    let tb = crate::table::csv(&txt);
                    if !tb.head.is_empty() {
                        return Raw::Tables(vec![(String::new(), tb)]);
                    }
                }
            }
            "json" | "geojson" | "xml" | "plist" | "opf" | "xsd" | "xsl" => {
                if let Some(txt) = read_limited(&job.path, 32 << 20) {
                    let tree = if ext.ends_with("json") { crate::tree::from_json(&txt) } else { crate::tree::from_xml(&txt) };
                    if let (Some(tree), Raw::Text(td)) = (tree, text_raw(&job.path)) {
                        return Raw::Tree(tree, td);
                    }
                }
            }
            "sqlite" | "sqlite3" | "db" | "db3" => {
                let mut m = [0u8; 16];
                if open_regular(&job.path).is_some_and(|mut f| f.read_exact(&mut m).is_ok()) && &m == b"SQLite format 3\0" {
                    return match crate::sqlite::read(&job.path) {
                        Ok(t) => Raw::Tables(t),
                        Err(e) => Raw::Err(e),
                    };
                }
            }
            e if crate::fontview::is_font_ext(e) => {
                return match crate::fontview::specimen(&job.path) {
                    Ok(d) => Raw::Image(d),
                    Err(e) => Raw::Err(e),
                }
            }
            _ => {}
        }
    }
    if let Some(ext) = job.path.extension().and_then(|e| e.to_str()) {
        if job.kind == Kind::Other && crate::mesh3d::is_mesh_ext(ext) && !remote {
            return match crate::mesh3d::load(&job.path) {
                Ok(m) => Raw::Mesh(m),
                // el motivo real (demasiado grande, dañado…) en vez de «sin vista previa»
                Err(e) => Raw::Err(tf("{}: {}", &[&t("No se pudo leer el modelo 3D"), &e])),
            };
        }
    }
    // archivo de un servidor SFTP: se descarga sólo si es pequeño
    if !matches!(job.kind, Kind::Folder) && crate::sftp::is_remote(&job.path) {
        let small = crate::vfs::fs::metadata(&job.path).map(|m| m.len() <= 32 << 20).unwrap_or(false);
        if !small || crate::sftp::ensure_local(&job.path).is_err() {
            return Raw::Binary;
        }
    }
    match job.kind {
        Kind::Folder => match crate::vfs::fs::read_dir(&job.path) {
            Ok(rd) => {
                let mut n = 0usize;
                let mut names = Vec::new();
                for e in rd.flatten() {
                    n += 1;
                    if names.len() < 600 {
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
            Err(e) => Raw::Err(tf("{}: {}", &[&t("No se pudo leer el comprimido"), &e])),
        },
        Kind::Image | Kind::Video => match decode_image(&job.path, MAX_IMG) {
            Some(d) => Raw::Image(d),
            // un vídeo sin fotograma sigue teniendo sus controles; una imagen que no se lee lo dice
            None if job.kind == Kind::Image => {
                let big = crate::vfs::fs::metadata(&job.path).map(|m| m.len() > 256 << 20).unwrap_or(false);
                Raw::Err(if big { t("La imagen es demasiado grande para mostrarla") } else { t("No se pudo leer la imagen (dañada o formato no compatible)") }.into())
            }
            None => Raw::Binary,
        },
        Kind::Text | Kind::Other => text_raw(&job.path),
        _ => Raw::Binary,
    }
}

/// Posición de lectura de un archivo visto hace poco (al volver a él se sigue donde estaba).
#[derive(Clone, Copy, Default)]
struct Memo {
    /// texto: primera línea visible
    text: f32,
    /// Markdown/libro: fracción recorrida
    rich: f32,
    table: (f32, f32),
    tree: f32,
    tree_text: bool,
    page: usize,
}

static MEMO: Mutex<Vec<(PathBuf, Memo)>> = Mutex::new(Vec::new());

fn recall(path: &std::path::Path) -> Memo {
    lock(&MEMO).iter().find(|(p, _)| p == path).map(|x| x.1).unwrap_or_default()
}

impl Preview {
    /// Guarda la posición de lectura (los últimos 24 archivos).
    fn remember(&self) {
        let mut m = Memo::default();
        match &self.body {
            Body::Text(_) => m.text = self.txt.top,
            Body::Rich => m.rich = self.rich.as_ref().map(|r| r.frac()).unwrap_or(0.0),
            Body::Table => m.table = self.table.as_ref().map(|t| (t.scroll, t.scroll_x)).unwrap_or_default(),
            Body::Tree(_) => {
                m.tree = self.tree.as_ref().map(|t| t.scroll).unwrap_or(0.0);
                m.tree_text = self.tree_text;
                m.text = self.txt.top;
            }
            Body::Doc => m.page = self.doc.as_ref().filter(|d| d.ready()).map(|d| d.page).or(self.want_page).unwrap_or(0),
            _ => return,
        }
        let mut g = lock(&MEMO);
        g.retain(|(p, _)| p != &self.path);
        g.push((self.path.clone(), m));
        if g.len() > 24 {
            g.remove(0);
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.remember();
        self.inner = None;
        if let Some(d) = self.tmp.take() {
            let _ = crate::vfs::fs::remove_dir_all(d);
        }
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
            audio: None,
            mesh: None,
            doc: None,
            rich: None,
            table: None,
            tree: None,
            tree_text: false,
            meta: Vec::new(),
            meta_rx: None,
            meta_started: false,
            show_meta: false,
            inner: None,
            entry_pick: None,
            tmp: None,
            entry_rx: None,
            entry_busy: None,
            entry_err: None,
            find_open: false,
            find_text: String::new(),
            sel: None,
            page_runs: None,
            doc_rect: None,
            sel_drag: false,
            last_pos: None,
            zoom: 1.0,
            pan: vec2(0.0, 0.0),
            pan_lim: vec2(0.0, 0.0),
            scroll: 0.0,
            txt: TextView::default(),
            nudge: 0.0,
            quick: false,
            hit_jump: false,
            want_page: None,
            opened_at: web_time::Instant::now(),
            doc_pending: false,
            // FB_USAGE (arnés de capturas): abre la pestaña de uso del disco
            #[cfg(feature = "selftest")]
            show_usage: std::env::var_os("FB_USAGE").is_some(),
            #[cfg(not(feature = "selftest"))]
            show_usage: false,
            usage: None,
        }
    }

    /// ¿El contenido se desplaza (texto, Markdown, tabla, árbol)? Entonces ↑/↓ lo mueven.
    pub fn scrollable(&self) -> bool {
        if let Some(i) = &self.inner {
            return i.0.scrollable();
        }
        matches!(self.body, Body::Text(_) | Body::Rich | Body::Table | Body::Tree(_))
    }

    /// Desplaza el contenido `dy` puntos (se aplica en el próximo dibujo).
    pub fn scroll_by(&mut self, dy: f32) {
        match &mut self.inner {
            Some(i) => i.0.scroll_by(dy),
            None => self.nudge += dy,
        }
    }

    /// Pausa el audio o el vídeo (al abrir la vista rápida encima del panel lateral).
    pub fn pause_media(&mut self) {
        if let Some(a) = self.audio.as_mut().filter(|a| a.playing()) {
            a.toggle();
        }
        if let Some(p) = self.player.as_mut().filter(|p| p.playing()) {
            p.toggle();
        }
    }

    pub fn player_active(&self) -> bool {
        self.player.as_ref().map(|p| p.playing()).unwrap_or(false) || self.audio.as_ref().map(|a| a.playing()).unwrap_or(false)
    }

    /// ¿Este visor puede reproducir su archivo (vídeo con ffmpeg o audio integrado)?
    pub fn can_play(&self) -> bool {
        match self.kind {
            Kind::Video => crate::video::have_ffmpeg(),
            Kind::Audio => crate::audio::supported(),
            _ => false,
        }
    }

    /// Crea el reproductor si hace falta y alterna reproducción/pausa.
    pub fn toggle_play(&mut self, ctx: &egui::Context) {
        if self.kind == Kind::Audio && crate::audio::supported() {
            let path = self.path.clone();
            self.audio.get_or_insert_with(|| crate::audio::AudioPlayer::new(ctx, &path)).toggle();
            return;
        }
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
        if let Some(a) = &mut self.audio {
            a.seek_by(d);
        }
    }

    pub fn poll(&mut self, w: &mut Worker, ctx: &egui::Context) {
        if let Some(rx) = &self.entry_rx {
            if let Ok(r) = rx.try_recv() {
                self.entry_rx = None;
                let name = self.entry_busy.take().unwrap_or_default();
                match r {
                    Ok(path) => {
                        let base = name.rsplit('/').next().unwrap_or(&name).to_string();
                        let ext = base.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
                        let kind = crate::fsys::kind_of_ext(&ext);
                        let size = crate::vfs::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        let mut iw = Worker::new(ctx);
                        let pv = Preview::open(&mut iw, path, base, kind, size, 0, String::new(), None);
                        self.inner = Some(Box::new((pv, iw)));
                    }
                    Err(e) => self.entry_err = Some(e),
                }
            }
        }
        if let Some(i) = &mut self.inner {
            i.0.poll(&mut i.1, ctx);
        }
        if self.kind == Kind::Audio && self.audio.is_none() && crate::audio::supported() {
            self.audio = Some(crate::audio::AudioPlayer::new(ctx, &self.path));
        }
        if let Some(a) = &mut self.audio {
            a.poll();
            if a.playing() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if let Some(p) = &mut self.player {
            p.poll();
            if p.playing() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if let Some(d) = &mut self.doc {
            d.poll(ctx);
        }
        // al pasar deprisa por muchos documentos no se abre ninguno: sólo el que se queda quieto un momento
        if self.doc_pending {
            let wait = std::time::Duration::from_millis(150).saturating_sub(self.opened_at.elapsed());
            if wait.is_zero() {
                self.doc_pending = false;
                self.doc = Some(crate::docview::DocView::new(ctx, &self.path));
            } else {
                ctx.request_repaint_after(wait);
            }
        }
        while let Ok((generation, raw)) = w.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.body = match raw {
                Raw::Image(d) => {
                    let img = crate::thumbs::color_image(ctx, d.w, d.h, &d.rgba);
                    Body::Image(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR))
                }
                Raw::Text(td) => {
                    self.txt = TextView::at(recall(&self.path).text);
                    Body::Text(td)
                }
                Raw::Folder(count, names) => Body::Folder { count, names },
                Raw::Archive(l) => {
                    let mut rows: Vec<(String, u64, bool)> = l.items.into_iter().map(|i| (i.name, i.size, i.dir)).collect();
                    rows.sort_by(|a, b| crate::fsys::natural_cmp(&a.0, &b.0));
                    Body::Archive { rows, truncated: l.truncated }
                }
                Raw::Mesh(m) => {
                    self.mesh = Some(MeshView { mesh: Arc::new(m), yaw: -0.6, pitch: 0.5, zoom: 1.0, tex: None, key: None, dragging: false, job: None, gpu: crate::gpu3d::shared() });
                    Body::Mesh
                }
                Raw::Doc => {
                    self.doc_pending = true;
                    self.want_page = Some(recall(&self.path).page).filter(|p| *p > 0);
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                    Body::Doc
                }
                Raw::Rich(r) => {
                    self.meta.extend(r.meta.iter().cloned());
                    let mut rv = crate::rich::RichView::new(r);
                    rv.set_frac(recall(&self.path).rich);
                    self.rich = Some(rv);
                    Body::Rich
                }
                Raw::Tables(t) => {
                    let mut tv = crate::table::TableView::new(t);
                    let m = recall(&self.path);
                    (tv.scroll, tv.scroll_x) = m.table;
                    self.table = Some(tv);
                    Body::Table
                }
                Raw::Tree(tree, td) => {
                    let mut tv = crate::tree::TreeView::new(tree);
                    let m = recall(&self.path);
                    tv.scroll = m.tree;
                    self.tree = Some(tv);
                    self.tree_text = m.tree_text;
                    self.txt = TextView::at(m.text);
                    Body::Tree(td)
                }
                Raw::Binary => Body::Info,
                Raw::Err(e) => Body::Error(e),
            };
        }
    }

    /// Dibuja el overlay. Devuelve la acción solicitada con el ratón.
    pub fn show(&mut self, ctx: &egui::Context, pal: &Pal, windowed: Option<bool>) -> Action {
        let screen = ctx.content_rect();
        let touch_full = windowed.is_none() && (cfg!(target_os = "android") || ctx.input(|i| i.any_touches()));
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("preview_layer"));
        let p = ctx.layer_painter(layer);
        let (card, rad) = if touch_full {
            // móvil: el visor ocupa toda la pantalla
            p.rect_filled(screen, 0.0, pal.panel);
            (screen, CornerRadius::ZERO)
        } else if let Some(rounded) = windowed {
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
        if self.kind != Kind::Folder && !crate::sftp::is_remote(&self.path) {
            let ib = Rect::from_center_size(pos2(close_r.left() - 26.0, head.center().y), vec2(34.0, 34.0));
            let over = pointer.map(|q| ib.contains(q)).unwrap_or(false);
            if over || self.show_meta {
                p.rect_filled(ib, CornerRadius::same(10), if self.show_meta { pal.sel } else { pal.hover });
            }
            if self.show_meta {
                p.rect_stroke(ib, CornerRadius::same(10), Stroke::new(1.0, sel_ring(pal)), egui::StrokeKind::Inside);
            }
            let ic = if self.show_meta { pal.accent } else { pal.text };
            p.circle_stroke(ib.center(), 9.0, Stroke::new(1.4, ic));
            p.text(ib.center(), Align2::CENTER_CENTER, "i", FontId::proportional(13.0), ic);
            if over && clicked {
                self.toggle_meta(ctx);
            }
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
            if touch_full {
                t("Atrás: cerrar   ·   Pellizca: zoom   ·   Doble toque: ajustar").to_string()
            } else if touch {
                t("Atrás o toque fuera: cerrar   ·   Pellizca: zoom   ·   Doble toque: ajustar").to_string()
            } else {
                // nombres de las teclas como en los menús (traducidos y con las convenciones de la plataforma)
                let k = crate::commands::display;
                let cmd = crate::commands::display_keys(egui::Modifiers::COMMAND, egui::Key::A);
                let cmd = cmd.strip_suffix("+A").unwrap_or("Ctrl");
                let find = matches!(self.body, Body::Text(_)) || (matches!(self.body, Body::Tree(_)) && self.tree_text);
                if self.scrollable() && self.inner.is_none() && self.player.is_none() {
                    let f = crate::commands::display_keys(egui::Modifiers::COMMAND, egui::Key::F);
                    if find {
                        tf("{} / {}: desplazar   ·   {} / {}: anterior / siguiente   ·   {}: buscar   ·   {}: cerrar", &[&k("ArrowUp"), &k("ArrowDown"), &k("ArrowLeft"), &k("ArrowRight"), &f, &k("Escape")])
                    } else {
                        tf("{} / {}: desplazar   ·   {} / {}: anterior / siguiente   ·   {}: cerrar", &[&k("ArrowUp"), &k("ArrowDown"), &k("ArrowLeft"), &k("ArrowRight"), &k("Escape")])
                    }
                } else if matches!(self.body, Body::Doc) {
                    tf("{} / {}: página   ·   Rueda: cambiar de página   ·   {}+rueda: zoom   ·   Flechas: anterior / siguiente archivo   ·   {}: cerrar", &[&k("PageUp"), &k("PageDown"), &cmd, &k("Escape")])
                } else if matches!(self.body, Body::Mesh) {
                    tf("Arrastrar: girar   ·   Rueda: zoom   ·   Doble clic: restablecer   ·   Flechas: anterior / siguiente   ·   {}: cerrar", &[&k("Escape")])
                } else if self.can_play() {
                    tf(
                        "{}: pausa / reanudar   ·   {} / {}: saltar 5 s   ·   {} / {}: anterior / siguiente   ·   {}: abrir fuera   ·   {}: cerrar",
                        &[&k("Space"), &k("ArrowLeft"), &k("ArrowRight"), &k("ArrowUp"), &k("ArrowDown"), &k("O"), &k("Escape")],
                    )
                } else {
                    tf("{} / {}: cerrar   ·   Flechas: anterior / siguiente   ·   {}: abrir   ·   Rueda: zoom   ·   Doble clic: ajustar", &[&k("Space"), &k("Escape"), &k("Enter")])
                }
            },
            FontId::proportional(11.5),
            pal.subtle,
        );

        let content = Rect::from_min_max(
            pos2(card.left() + 14.0, head.bottom() + 12.0),
            pos2(card.right() - 14.0, card.bottom() - foot_h),
        );

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

        self.quick = true;
        self.draw_body(ctx, &p, content, pal, pointer, clicked, true, true);
        if self.show_meta {
            self.draw_meta(ctx, &p, content, pal);
        }
        action
    }

    /// Dibuja la vista previa acoplada (panel lateral) dentro de `r`, sin velo ni cierre.
    pub fn show_panel(&mut self, ctx: &egui::Context, p: &egui::Painter, r: Rect, pal: &Pal, active: bool) {
        let p = &p.with_clip_rect(r);
        let head = Rect::from_min_size(r.min, vec2(r.width(), 52.0));
        let icon_r = Rect::from_center_size(pos2(head.left() + 28.0, head.center().y), vec2(30.0, 30.0));
        if self.kind == Kind::Folder {
            theme::draw_folder(p, icon_r);
        } else {
            let ext = self.name.rsplit_once('.').map(|x| x.1).unwrap_or("");
            theme::draw_file(p, icon_r, self.kind, ext, pal);
        }
        let tx = head.left() + 52.0;
        let maxw = head.width() - 52.0 - 46.0;
        let mut title: String = self.name.clone();
        let g = p.layout_no_wrap(title.clone(), FontId::proportional(15.0), pal.text);
        if g.size().x > maxw {
            let keep = ((maxw / g.size().x) * title.chars().count() as f32) as usize;
            title = title.chars().take(keep.saturating_sub(2)).collect::<String>() + "…";
        }
        p.text(pos2(tx, head.center().y - 8.0), Align2::LEFT_CENTER, title, FontId::proportional(15.0), pal.text);
        let sub = if self.kind == Kind::Folder { self.kind.label().to_string() } else { format!("{}  ·  {}", self.kind.label(), human_size(self.size)) };
        p.text(pos2(tx, head.center().y + 10.0), Align2::LEFT_CENTER, sub, FontId::proportional(11.5), pal.subtle);
        let live = if active { ctx.input(|i| i.pointer.hover_pos()) } else { None };
        let clicked = active && ctx.input(|i| i.pointer.primary_clicked());
        let pointer = live.filter(|q| r.contains(*q));
        if self.kind != Kind::Folder && !crate::sftp::is_remote(&self.path) {
            let ib = Rect::from_center_size(pos2(head.right() - 26.0, head.center().y), vec2(30.0, 30.0));
            let over = pointer.map(|q| ib.contains(q)).unwrap_or(false);
            if over || self.show_meta {
                p.rect_filled(ib, CornerRadius::same(9), if self.show_meta { pal.sel } else { pal.hover });
            }
            if self.show_meta {
                p.rect_stroke(ib, CornerRadius::same(9), Stroke::new(1.0, sel_ring(pal)), egui::StrokeKind::Inside);
            }
            let ic = if self.show_meta { pal.accent } else { pal.text };
            p.circle_stroke(ib.center(), 8.0, Stroke::new(1.4, ic));
            p.text(ib.center(), Align2::CENTER_CENTER, "i", FontId::proportional(12.0), ic);
            if over && clicked {
                self.toggle_meta(ctx);
            }
        }
        p.line_segment([pos2(r.left() + 12.0, head.bottom()), pos2(r.right() - 12.0, head.bottom())], Stroke::new(1.0, pal.border));
        let content = Rect::from_min_max(pos2(r.left() + 10.0, head.bottom() + 8.0), pos2(r.right() - 10.0, r.bottom() - 10.0));
        self.quick = false;
        // el teclado (páginas, Ctrl+F, copiar) sólo con el puntero encima del panel
        self.draw_body(ctx, p, content, pal, pointer, clicked, active, active && pointer.is_some());
        if self.show_meta {
            self.draw_meta(ctx, p, content, pal);
        }
    }

    /// Cierra el archivo abierto dentro del comprimido; `false` si no había ninguno.
    pub fn close_inner(&mut self) -> bool {
        self.entry_err = None;
        self.entry_rx = None;
        self.entry_busy = None;
        self.inner.take().is_some()
    }

    fn open_entry(&mut self, ctx: &egui::Context, name: String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (src, ctx2, n) = (self.path.clone(), ctx.clone(), name.clone());
        let stamp = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = dirs::cache_dir().unwrap_or_else(crate::vfs::temp_dir).join("filebrowser").join(format!("inner-{}-{stamp}", crate::vfs::pid()));
        if let Some(old) = self.tmp.replace(dir.clone()) {
            let _ = crate::vfs::fs::remove_dir_all(old);
        }
        self.entry_err = None;
        crate::thumbs::spawn_decoder("entrada", move || {
            let r = crate::thumbs::guarded(|| crate::archive::read_entry(&src, &n, &dir, 64 << 20)).unwrap_or_else(|| Err(t("No se pudo abrir").into()));
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.entry_rx = Some(rx);
        self.entry_busy = Some(name);
    }

    /// ¿Está escribiendo en el cuadro de búsqueda del documento?
    pub fn typing(&self) -> bool {
        self.find_open
    }

    /// ¿Hay texto seleccionado en el visor de texto? (Ctrl+C copia eso y no los archivos)
    pub fn has_text_sel(&self) -> bool {
        self.txt.sel.is_some_and(|(a, b)| a != b)
    }

    /// Esc dentro de la búsqueda la cierra; `false` si no estaba abierta.
    pub fn close_find(&mut self) -> bool {
        if !self.find_open {
            return false;
        }
        self.find_open = false;
        self.hit_jump = false;
        self.txt.hits.clear();
        if let Some(d) = &mut self.doc {
            d.close_find();
        }
        true
    }

    pub fn toggle_meta(&mut self, ctx: &egui::Context) {
        self.show_meta = !self.show_meta;
        if self.show_meta && !self.meta_started {
            self.meta_started = true;
            let (tx, rx) = std::sync::mpsc::channel();
            let (path, kind, size, ctx2) = (self.path.clone(), self.kind, self.size, ctx.clone());
            crate::thumbs::spawn_decoder("metadatos", move || {
                let _ = tx.send(crate::thumbs::guarded(|| crate::meta::collect(&path, kind, size)).unwrap_or_default());
                ctx2.request_repaint();
            });
            self.meta_rx = Some(rx);
        }
    }

    fn draw_meta(&mut self, _ctx: &egui::Context, p: &egui::Painter, content: Rect, pal: &Pal) {
        if let Some(rx) = &self.meta_rx {
            if let Ok(rows) = rx.try_recv() {
                let extra = std::mem::take(&mut self.meta);
                self.meta = rows;
                self.meta.extend(extra);
                self.meta_rx = None;
            }
        }
        let w = 300.0f32.min(content.width() - 20.0);
        let rows = &self.meta;
        let h = (46.0 + rows.len() as f32 * 38.0).min(content.height() - 20.0).max(70.0);
        let r = Rect::from_min_size(pos2(content.right() - w - 10.0, content.top() + 10.0), vec2(w, h));
        p.rect_filled(r, CornerRadius::same(12), pal.panel.gamma_multiply(0.96));
        p.rect_stroke(r, CornerRadius::same(12), Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
        p.text(pos2(r.left() + 14.0, r.top() + 18.0), Align2::LEFT_CENTER, t("Información"), FontId::proportional(14.0), pal.text);
        let cp = p.with_clip_rect(r);
        if self.meta_rx.is_some() {
            cp.text(r.center(), Align2::CENTER_CENTER, t("Cargando…"), FontId::proportional(12.0), pal.subtle);
            return;
        }
        if rows.is_empty() {
            cp.text(r.center(), Align2::CENTER_CENTER, t("Sin datos"), FontId::proportional(12.0), pal.subtle);
            return;
        }
        let mut y = r.top() + 40.0;
        for (k, v) in rows {
            if y + 30.0 > r.bottom() {
                break;
            }
            cp.text(pos2(r.left() + 14.0, y), Align2::LEFT_TOP, k, FontId::proportional(11.0), pal.subtle);
            let mut val = v.clone();
            if val.chars().count() > 38 {
                val = val.chars().take(37).collect::<String>() + "…";
            }
            cp.text(pos2(r.left() + 14.0, y + 14.0), Align2::LEFT_TOP, val, FontId::proportional(12.5), pal.text);
            y += 38.0;
        }
    }

    /// Contenido de la tarjeta (todo salvo la cabecera y el pie); también lo usa la vista previa anidada.
    /// `active`: acepta el ratón (no hay otro visor encima); `keys`: el teclado es suyo.
    #[allow(clippy::too_many_arguments)]
    fn draw_body(&mut self, ctx: &egui::Context, p: &egui::Painter, content: Rect, pal: &Pal, pointer: Option<Pos2>, clicked: bool, active: bool, keys: bool) {
        if self.inner.is_some() {
            let bar = Rect::from_min_size(content.min, vec2(content.width(), 30.0));
            let hot = pointer.map(|q| bar.contains(q)).unwrap_or(false);
            p.rect_filled(bar, CornerRadius::same(8), if hot { pal.hover } else { pal.surface });
            let nm = self.inner.as_ref().map(|i| i.0.name.clone()).unwrap_or_default();
            p.text(pos2(bar.left() + 12.0, bar.center().y), Align2::LEFT_CENTER, format!("‹  {}  ·  {}", t("Volver al comprimido"), nm), FontId::proportional(13.0), pal.text);
            if hot && clicked {
                self.close_inner();
                return;
            }
            let rest = Rect::from_min_max(pos2(content.left(), content.top() + 36.0), content.max);
            if let Some(i) = &mut self.inner {
                i.0.quick = self.quick;
                i.0.draw_body(ctx, p, rest, pal, pointer, clicked, active, keys);
            }
            return;
        }
        let cp = p.with_clip_rect(content);
        let in_content = active && pointer.map(|q| content.contains(q)).unwrap_or(false);
        let nudge = std::mem::take(&mut self.nudge);
        let click = clicked.then_some(pointer).flatten();
        // búsqueda en el texto plano (Ctrl+F); el cuadro se dibuja encima al final
        let text_find = matches!(self.body, Body::Text(_)) || (matches!(self.body, Body::Tree(_)) && self.tree_text);
        if text_find && keys && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            self.find_open = true;
        }
        if text_find && self.find_open && keys {
            let (changed, step) = self.find_keys(ctx);
            if let Body::Text(td) | Body::Tree(td) = &self.body {
                if changed {
                    self.txt.search(td, &self.find_text);
                }
            }
            if step != 0 {
                self.txt.step_hit(step);
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
                // «Cargando…» para todos los tipos mientras llega
                p.text(pos2(content.center().x, content.bottom() - 14.0), Align2::CENTER_CENTER, t("Cargando…"), FontId::proportional(12.0), pal.subtle);
            }
            Body::Text(td) => {
                let find = self.find_open.then(|| find_rect(content));
                draw_text(ctx, &cp, content, in_content, active, keys, self.quick, &mut self.txt, td, nudge, click, find, pal);
            }
            Body::Rich => {
                if let Some(r) = &mut self.rich {
                    r.scroll += nudge;
                    r.draw(ctx, &cp, content, pal, in_content, active, keys);
                }
            }
            Body::Table => {
                if let Some(tv) = &mut self.table {
                    tv.scroll += nudge;
                    tv.draw(ctx, &cp, content, pal, in_content, pointer, clicked, active, keys);
                }
            }
            Body::Tree(td) => {
                for (i, label) in [t("Árbol"), t("Texto")].into_iter().enumerate() {
                    let r = Rect::from_min_size(pos2(content.left() + 18.0 + i as f32 * 98.0, content.top() + 8.0), vec2(90.0, 28.0));
                    let on = self.tree_text == (i == 1);
                    let hot = pointer.map(|q| r.contains(q)).unwrap_or(false);
                    cp.rect_filled(r, CornerRadius::same(8), if on { pal.sel } else if hot { pal.hover } else { pal.surface });
                    if on {
                        cp.rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, sel_ring(pal)), egui::StrokeKind::Inside);
                    }
                    cp.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), if on { pal.accent } else { pal.text });
                    if hot && clicked {
                        self.tree_text = i == 1;
                    }
                }
                let area = Rect::from_min_max(pos2(content.left(), content.top() + 44.0), content.max);
                if self.tree_text {
                    let find = self.find_open.then(|| find_rect(area));
                    draw_text(ctx, &cp.with_clip_rect(area), area, in_content, active, keys, self.quick, &mut self.txt, td, nudge, click, find, pal);
                } else if let Some(tv) = &mut self.tree {
                    tv.scroll += nudge;
                    tv.draw(ctx, &cp, area, pal, in_content, pointer, clicked, active, keys);
                }
            }
            Body::Folder { count, names } => {
                // pestañas: contenido / uso de disco
                for (i, label) in [t("Contenido"), t("Uso de disco")].into_iter().enumerate() {
                    let r = Rect::from_min_size(pos2(content.left() + 18.0 + i as f32 * 118.0, content.top() + 8.0), vec2(110.0, 28.0));
                    let on = self.show_usage == (i == 1);
                    let hot = pointer.map(|q| r.contains(q)).unwrap_or(false);
                    cp.rect_filled(r, CornerRadius::same(8), if on { pal.sel } else if hot { pal.hover } else { pal.surface });
                    if on {
                        cp.rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, sel_ring(pal)), egui::StrokeKind::Inside);
                    }
                    cp.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), if on { pal.accent } else { pal.text });
                    if hot && clicked {
                        self.show_usage = i == 1;
                    }
                }
                let area = Rect::from_min_max(pos2(content.left(), content.top() + 44.0), content.max);
                if self.show_usage {
                    if self.usage.is_none() {
                        self.usage = Some(UsageView {
                            scan: crate::diskusage::start(self.path.clone(), ctx, true),
                            root: None,
                            path: Vec::new(),
                        });
                    }
                    if let Some(uv) = &mut self.usage {
                        usage_ui(&cp, pal, ctx, area, self.path.clone(), uv, pointer, clicked);
                    }
                } else {
                    cp.text(pos2(content.left() + 18.0, area.top() + 12.0), Align2::LEFT_CENTER, tf("{count} elementos", &[count]), FontId::proportional(13.0), pal.text);
                    let list = Rect::from_min_max(pos2(area.left(), area.top() + 28.0), area.max);
                    let (shown, _) = name_columns(&cp, pal, list, names.len(), |i| (names[i].as_str(), None, false), None);
                    if *count > shown {
                        cp.text(pos2(content.center().x, content.bottom() - 12.0), Align2::CENTER_CENTER, format!("… +{}", count - shown), FontId::proportional(12.0), pal.subtle);
                    }
                }
            }
            Body::Archive { rows, truncated } => {
                let head = if *truncated { format!("{} {}", tf("{count} elementos", &[&rows.len()]), t("(índice parcial)")) } else { tf("{count} elementos", &[&rows.len()]) };
                cp.text(pos2(content.left() + 18.0, content.top() + 16.0), Align2::LEFT_CENTER, head, FontId::proportional(13.0), pal.text);
                let list = Rect::from_min_max(pos2(content.left(), content.top() + 32.0), pos2(content.right(), content.bottom() - 22.0));
                let hover = if self.entry_busy.is_none() { pointer } else { None };
                let (fit, hit) = name_columns(&cp, pal, list, rows.len(), |i| (rows[i].0.as_str(), (!rows[i].2).then_some(rows[i].1), rows[i].2), hover);
                if let (Some(i), true) = (hit, clicked) {
                    if !rows[i].2 {
                        self.entry_pick = Some(rows[i].0.clone());
                    }
                }
                if let Some(n) = &self.entry_busy {
                    cp.text(pos2(content.center().x, content.bottom() - 12.0), Align2::CENTER_CENTER, format!("{} {n}", t("Abriendo")), FontId::proportional(12.0), pal.accent);
                } else if let Some(e) = &self.entry_err {
                    cp.text(pos2(content.center().x, content.bottom() - 12.0), Align2::CENTER_CENTER, e, FontId::proportional(12.0), pal.danger);
                } else if rows.len() > fit {
                    cp.text(
                        pos2(content.center().x, content.bottom() - 12.0),
                        Align2::CENTER_CENTER,
                        format!("… +{}", rows.len() - fit),
                        FontId::proportional(12.0),
                        pal.subtle,
                    );
                }
            }
            Body::Mesh => self.draw_mesh(ctx, &cp, content, pal, in_content),
            Body::Doc => self.draw_doc(ctx, &cp, content, pal, in_content, click, keys),
            Body::Info if self.kind == Kind::Audio => {
                let c = pos2(content.center().x, content.center().y - 50.0);
                let r = Rect::from_center_size(c, vec2(180.0, 180.0));
                let cover = self.audio.as_ref().and_then(|a| a.tex.as_ref().map(|t| (t.id(), t.size_vec2())));
                if let Some((id, sz)) = cover {
                    let s = (180.0 / sz.x).min(180.0 / sz.y);
                    let rr = Rect::from_center_size(c, sz * s);
                    cp.add(egui::epaint::RectShape::filled(rr, CornerRadius::same(10), Color32::WHITE).with_texture(id, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))));
                } else {
                    let ext = self.name.rsplit_once('.').map(|x| x.1).unwrap_or("");
                    theme::draw_file(&cp, r, self.kind, ext, pal);
                }
                let (title, artist, album) = match &self.audio {
                    Some(a) => (a.title.clone(), a.artist.clone(), a.album.clone()),
                    None => Default::default(),
                };
                let title = if title.is_empty() { self.name.clone() } else { title };
                let mut y = c.y + 108.0;
                cp.text(pos2(c.x, y), Align2::CENTER_CENTER, title, FontId::proportional(16.0), pal.text);
                for s in [artist, album] {
                    if !s.is_empty() {
                        y += 22.0;
                        cp.text(pos2(c.x, y), Align2::CENTER_CENTER, s, FontId::proportional(13.0), pal.subtle);
                    }
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
        if text_find && self.find_open {
            let area = if matches!(self.body, Body::Tree(_)) { Rect::from_min_max(pos2(content.left(), content.top() + 44.0), content.max) } else { content };
            let info = (self.txt.cur, self.txt.hits.len(), true);
            let step = self.draw_find_bar(ctx, &cp, area, pal, click, info);
            if step != 0 {
                self.txt.step_hit(step);
            }
        }
        if let Some(n) = self.entry_pick.take() {
            self.open_entry(ctx, n);
        }
        if matches!(self.kind, Kind::Video | Kind::Audio) {
            self.video_controls(ctx, p, content, pal, pointer, clicked);
        }
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
        let audio = self.kind == Kind::Audio;
        if !self.can_play() {
            let msg = if audio { t("La reproducción de audio no está disponible en esta plataforma") } else { t("Para reproducir vídeo hace falta ffmpeg") };
            p.text(pos2(content.center().x, content.bottom() - 30.0), Align2::CENTER_CENTER, msg, FontId::proportional(12.5), pal.subtle);
            return;
        }
        let (playing, pos, dur, err) = if audio {
            match &self.audio {
                Some(a) => (a.playing(), a.position(), a.duration, a.error.clone()),
                None => (false, 0.0, 0.0, None),
            }
        } else {
            match &self.player {
                Some(pl) => (pl.playing(), pl.position(), pl.duration(), pl.error.clone()),
                None => (false, 0.0, 0.0, None),
            }
        };
        let hit = |r: Rect| pointer.map(|q| r.contains(q)).unwrap_or(false) && clicked;
        let mut do_toggle = false;
        let mut do_seek: Option<f64> = None;
        // botón grande central mientras no hay reproducción iniciada
        if self.player.is_none() && !audio {
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
                tf("{}: {}", &[&if audio { t("No se pudo reproducir el audio") } else { t("No se pudo reproducir el vídeo") }, &e]),
                FontId::proportional(12.5),
                pal.danger,
            );
        }
        if do_toggle {
            self.toggle_play(ctx);
        } else if let Some(s) = do_seek {
            let path = self.path.clone();
            if audio {
                self.audio.get_or_insert_with(|| crate::audio::AudioPlayer::new(ctx, &path)).seek_to(s);
            } else {
                let pl = self.player.get_or_insert_with(|| crate::video::Player::new(ctx, &path));
                pl.seek_to(s);
            }
        }
    }

    /// Modelo 3D: arrastrar gira, la rueda acerca, doble clic restablece.
    fn draw_mesh(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, pal: &Pal, hot: bool) {
        let dark = pal.bg.r() < 128;
        let Some(mv) = &mut self.mesh else { return };
        let (dy, drag, dbl, down) = ctx.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.pointer.delta(),
                i.pointer.button_double_clicked(egui::PointerButton::Primary),
                i.pointer.primary_down() && !edge_start(i),
            )
        });
        let pinch = ctx.input(|i| i.zoom_delta());
        let mut moving = false;
        if hot {
            if down && (drag.x != 0.0 || drag.y != 0.0) {
                mv.yaw += drag.x * 0.01;
                mv.pitch = (mv.pitch + drag.y * 0.01).clamp(-1.55, 1.55);
                moving = true;
            }
            if dy != 0.0 {
                mv.zoom = (mv.zoom * (dy * 0.0015).exp()).clamp(0.2, 20.0);
            }
            if pinch != 1.0 {
                mv.zoom = (mv.zoom * pinch).clamp(0.2, 20.0);
            }
            if dbl {
                mv.yaw = -0.6;
                mv.pitch = 0.5;
                mv.zoom = 1.0;
            }
        }
        mv.dragging = moving || (hot && down && mv.dragging);
        if !crate::gpu3d::failed(&mv.gpu) {
            // GPU: fondo y luego la malla en un callback de OpenGL
            p.rect_filled(area, CornerRadius::same(10), pal.bg);
            let base = if dark { [150.0 / 255.0, 185.0 / 255.0, 1.0] } else { [70.0 / 255.0, 110.0 / 255.0, 220.0 / 255.0] };
            let cam = crate::gpu3d::Cam { yaw: mv.yaw, pitch: mv.pitch, zoom: mv.zoom, base };
            p.add(crate::gpu3d::callback(area, mv.mesh.clone(), mv.gpu.clone(), cam));
            if crate::gpu3d::pending(&mv.gpu) {
                ctx.request_repaint(); // si la GPU falla en la primera pasada se pasa al software
            }
            let m = &mv.mesh;
            let info = tf(
                "{} triángulos · {} × {} × {}",
                &[&m.count(), &format!("{:.4}", m.size[0]).trim_end_matches('0').trim_end_matches('.'), &format!("{:.4}", m.size[1]).trim_end_matches('0').trim_end_matches('.'), &format!("{:.4}", m.size[2]).trim_end_matches('0').trim_end_matches('.')],
            );
            p.text(pos2(area.left() + 12.0, area.bottom() - 10.0), Align2::LEFT_BOTTOM, info, FontId::proportional(12.0), pal.subtle);
            return;
        }
        // al arrastrar se dibuja a media resolución y con menos triángulos
        let full = !mv.dragging;
        let div = if full { 1.0 } else { 2.0 };
        let rw = ((area.width() / div) as usize).clamp(64, 900);
        let rh = (((area.height() / div) * rw as f32 / (area.width() / div).max(1.0)) as usize).clamp(64, 900);
        let key: MeshKey = (rw, rh, (mv.yaw * 1000.0) as i32, (mv.pitch * 1000.0) as i32, (mv.zoom * 1000.0) as i32, dark, full);
        // ¿ha terminado el dibujo en curso?
        let mut done = None;
        if let Some((k, rx)) = &mv.job {
            match rx.try_recv() {
                Ok(rgba) => done = Some((*k, rgba)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => mv.job = None,
                Err(_) => {}
            }
        }
        if let Some((k, rgba)) = done {
            mv.job = None;
            let img = egui::ColorImage::from_rgba_unmultiplied([k.0, k.1], &rgba);
            match &mut mv.tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => mv.tex = Some(ctx.load_texture("mesh", img, egui::TextureOptions::LINEAR)),
            }
            mv.key = Some(k);
        }
        if mv.key != Some(key) && mv.job.is_none() {
            let (bg, base) = if dark { ([pal.bg.r(), pal.bg.g(), pal.bg.b()], [150, 185, 255]) } else { ([pal.bg.r(), pal.bg.g(), pal.bg.b()], [70, 110, 220]) };
            let (tx, rx) = std::sync::mpsc::channel();
            let (mesh, ctx2, yaw, pitch, zoom) = (mv.mesh.clone(), ctx.clone(), mv.yaw, mv.pitch, mv.zoom);
            crate::thumbs::spawn_decoder("modelo 3D", move || {
                // si el dibujo falla se manda el fondo liso: así no se vuelve a pedir en cada fotograma
                let img = crate::thumbs::guarded(|| crate::mesh3d::render(&mesh, yaw, pitch, zoom, rw, rh, bg, base, full))
                    .unwrap_or_else(|| [bg[0], bg[1], bg[2], 255].repeat(rw * rh));
                let _ = tx.send(img);
                ctx2.request_repaint();
            });
            mv.job = Some((key, rx));
        }
        if mv.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if let Some(t) = &mv.tex {
            let shape = egui::epaint::RectShape::filled(area, CornerRadius::same(10), Color32::WHITE)
                .with_texture(t.id(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
            p.add(shape);
        }
        let m = &mv.mesh;
        let info = tf(
            "{} triángulos · {} × {} × {}",
            &[&m.count(), &format!("{:.4}", m.size[0]).trim_end_matches('0').trim_end_matches('.'), &format!("{:.4}", m.size[1]).trim_end_matches('0').trim_end_matches('.'), &format!("{:.4}", m.size[2]).trim_end_matches('0').trim_end_matches('.')],
        );
        p.text(pos2(area.left() + 12.0, area.bottom() - 10.0), Align2::LEFT_BOTTOM, info, FontId::proportional(12.0), pal.subtle);
        if mv.dragging || hot {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// Teclado del cuadro de búsqueda abierto: (texto cambiado, salto ±1 con Intro / Mayús+Intro).
    fn find_keys(&mut self, ctx: &egui::Context) -> (bool, isize) {
        let (txt, back, enter, shift) = ctx.input_mut(|i| {
            let mut t = String::new();
            for e in &i.events {
                if let egui::Event::Text(s) = e {
                    t.push_str(s);
                }
            }
            let shift = i.modifiers.shift;
            (t, i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace), i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) || i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter), shift)
        });
        let mut changed = false;
        if !txt.is_empty() {
            self.find_text.push_str(&txt.replace(['\n', '\r'], ""));
            changed = true;
        }
        if back && self.find_text.pop().is_some() {
            changed = true;
        }
        (changed, if enter { if shift { -1 } else { 1 } } else { 0 })
    }

    /// Documento paginado: página ajustada al área, con zoom, y barra de páginas abajo.
    #[allow(clippy::too_many_arguments)]
    fn draw_doc(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, pal: &Pal, hot: bool, click: Option<Pos2>, keys: bool) {
        let ppp = ctx.pixels_per_point();
        let (dy, ctrl, pg, home, end) = ctx.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.modifiers.command,
                if keys { i.key_pressed(egui::Key::PageDown) as isize - i.key_pressed(egui::Key::PageUp) as isize } else { 0 },
                keys && i.key_pressed(egui::Key::Home),
                keys && i.key_pressed(egui::Key::End),
            )
        });
        if self.doc.is_none() {
            // aún sin abrir (se espera a que la selección se quede quieta)
            p.text(area.center(), Align2::CENTER_CENTER, t("Cargando documento…"), FontId::proportional(14.0), pal.subtle);
            return;
        }
        // Ctrl+F abre la búsqueda; mientras está abierta el teclado es suyo
        if keys && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            self.find_open = true;
        }
        let (mut find_changed, mut hit_step) = (false, 0isize);
        if self.find_open && keys {
            (find_changed, hit_step) = self.find_keys(ctx);
        }
        let doc = self.doc.as_mut().unwrap();
        doc.poll(ctx);
        if doc.ready() {
            if let Some(pg) = self.want_page.take() {
                doc.goto(pg);
            }
        }
        if find_changed {
            let q = self.find_text.clone();
            doc.search(&q);
            self.hit_jump = true;
        }
        if hit_step != 0 {
            doc.next_hit(hit_step);
            self.hit_jump = true;
        }
        let step = if self.find_open { 0 } else { pg };
        // con la página ampliada la rueda la recorre; al llegar al borde, sigue a la página de al lado
        let mut wheel_step = 0isize;
        if hot && !ctrl && dy != 0.0 {
            let lim = self.pan_lim.y;
            let before = self.pan.y;
            if lim > 1.0 {
                self.pan.y = (self.pan.y + dy).clamp(-lim, lim);
            }
            if lim > 1.0 && (self.pan.y - before).abs() > 0.1 {
                self.scroll = 0.0;
            } else {
                self.scroll += dy;
                if self.scroll.abs() >= 60.0 {
                    wheel_step = -(self.scroll / 60.0).trunc() as isize;
                    self.scroll = 0.0;
                }
            }
        }
        if step != 0 {
            doc.step(step);
            self.zoom = 1.0;
            self.pan = vec2(0.0, 0.0);
        } else if wheel_step != 0 {
            let before = doc.page;
            doc.step(wheel_step);
            if doc.page != before {
                // se conserva el zoom: la página nueva empieza por arriba (o por abajo si se sube)
                self.pan.y = if wheel_step > 0 { self.pan_lim.y } else { -self.pan_lim.y };
            }
        }
        if home && !self.find_open {
            doc.goto(0);
        }
        if end && !self.find_open {
            doc.goto(usize::MAX);
        }
        // dibuja con la resolución necesaria para el zoom actual
        doc.want(area.width() * ppp * self.zoom.max(1.0));
        let tex = doc.texture().map(|t| (t.id(), t.size_vec2()));
        let shown = doc.tex_page();
        let (count, page, error, busy) = (doc.count, doc.page, doc.shown_error(), doc.busy());
        // texto de la página (para seleccionar y copiar)
        if let Some(r) = doc.runs() {
            if self.page_runs.as_ref().map(|x| x.0) != Some(page) {
                self.page_runs = Some((page, r.to_vec()));
            }
        }
        let mut nav = 0isize;
        let mut page_rect = None;
        if let Some((id, sz)) = tex.filter(|_| error.is_none()) {
            page_rect = Some(self.draw_zoomable(ctx, p, area, id, sz, hot));
        }
        self.doc_rect = page_rect.filter(|_| shown == Some(page));
        // la coincidencia actual, dentro de la vista aunque la página esté ampliada
        if self.hit_jump {
            let hit = self.doc.as_ref().and_then(|d| d.find.as_ref()).and_then(|f| f.hits.get(f.cur).copied());
            match (hit, self.doc_rect) {
                (Some(h), Some(pr)) if h.page == page => {
                    let hr = Rect::from_min_size(pos2(pr.left() + h.x * pr.width(), pr.top() + h.y * pr.height()), vec2(h.w * pr.width(), h.h * pr.height()));
                    if !area.shrink(24.0).contains_rect(hr) {
                        self.pan += area.center() - hr.center();
                        ctx.request_repaint();
                    }
                    self.hit_jump = false;
                }
                (None, _) if self.doc.as_ref().and_then(|d| d.find.as_ref()).map(|f| f.done).unwrap_or(true) => self.hit_jump = false,
                _ => {}
            }
        }
        if let Some(e) = error {
            p.text(area.center(), Align2::CENTER_CENTER, e, FontId::proportional(15.0), pal.subtle);
        } else if tex.is_none() {
            p.text(area.center(), Align2::CENTER_CENTER, t("Cargando documento…"), FontId::proportional(14.0), pal.subtle);
        }
        if let Some(pr) = self.doc_rect {
            self.doc_text_layer(ctx, p, pr, hot, page, keys);
        }
        if count > 1 {
            let label = format!("{} / {}", page + 1, count);
            let w = 150.0;
            let bar = Rect::from_center_size(pos2(area.center().x, area.bottom() - 22.0), vec2(w, 30.0));
            p.rect_filled(bar, CornerRadius::same(15), Color32::from_black_alpha(150));
            p.text(bar.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), Color32::WHITE);
            for (dx, glyph, d) in [(-w / 2.0 + 18.0, "‹", -1isize), (w / 2.0 - 18.0, "›", 1isize)] {
                let r = Rect::from_center_size(pos2(bar.center().x + dx, bar.center().y), vec2(30.0, 30.0));
                if click.map(|q| r.contains(q)).unwrap_or(false) {
                    nav = d;
                }
                p.text(r.center() + vec2(0.0, -1.5), Align2::CENTER_CENTER, glyph, FontId::proportional(22.0), Color32::WHITE);
            }
        }
        if nav != 0 {
            if let Some(doc) = &mut self.doc {
                doc.step(nav);
            }
            self.zoom = 1.0;
            self.pan = vec2(0.0, 0.0);
        }
        if self.find_open {
            let info = match self.doc.as_ref().and_then(|d| d.find.as_ref()) {
                Some(f) => (f.cur, f.hits.len(), f.done),
                None => (0, 0, true),
            };
            let step = self.draw_find_bar(ctx, p, area, pal, click, info);
            if step != 0 {
                if let Some(d) = &mut self.doc {
                    d.next_hit(step);
                }
                self.hit_jump = true;
            }
        }
        if busy || self.hit_jump || self.doc.as_ref().and_then(|d| d.find.as_ref()).map(|f| !f.done).unwrap_or(false) {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }

    /// Coincidencias de la búsqueda, selección con el ratón y copia del texto de la página.
    fn doc_text_layer(&mut self, ctx: &egui::Context, p: &egui::Painter, pr: Rect, hot: bool, page: usize, keys: bool) {
        let to_screen = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(pr.left() + x * pr.width(), pr.top() + y * pr.height()), vec2(w * pr.width(), h * pr.height()));
        if let Some(f) = self.doc.as_ref().and_then(|d| d.find.as_ref()) {
            for (k, h) in f.hits.iter().enumerate().filter(|(_, h)| h.page == page) {
                let col = if k == f.cur { Color32::from_rgba_unmultiplied(255, 140, 0, 120) } else { Color32::from_rgba_unmultiplied(255, 220, 0, 90) };
                p.rect_filled(to_screen(h.x, h.y, h.w, h.h).expand(1.0), CornerRadius::same(2), col);
            }
        }
        let runs: &[crate::docview::Run] = match &self.page_runs {
            Some((pg, r)) if *pg == page => r,
            _ => &[],
        };
        // posición (trozo, carácter) bajo el puntero
        let at = |q: Pos2, near: bool| -> Option<(usize, usize)> {
            let (fx, fy) = ((q.x - pr.left()) / pr.width(), (q.y - pr.top()) / pr.height());
            let mut best: Option<(usize, f32)> = None;
            for (i, r) in runs.iter().enumerate() {
                let dy = if fy < r.y { r.y - fy } else if fy > r.y + r.h { fy - (r.y + r.h) } else { 0.0 };
                let dx = if fx < r.x { r.x - fx } else if fx > r.x + r.w { fx - (r.x + r.w) } else { 0.0 };
                let d = dy * 4.0 + dx;
                if best.map(|b| d < b.1).unwrap_or(true) {
                    best = Some((i, d));
                }
            }
            let (i, d) = best?;
            if !near && d > 0.0 {
                return None;
            }
            let r = &runs[i];
            let n = r.text.chars().count();
            let c = (((fx - r.x) / r.w.max(1e-6)) * n as f32).round().clamp(0.0, n as f32) as usize;
            Some((i, c))
        };
        let (pressed, down, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.latest_pos()));
        // con el dedo, arrastrar mueve la página en vez de seleccionar
        let touch = cfg!(target_os = "android") || ctx.input(|i| i.any_touches());
        if hot && pressed && !touch {
            self.sel = None;
            self.sel_drag = false;
            if let Some(q) = pos.filter(|q| pr.contains(*q)) {
                if let Some(a) = at(q, false) {
                    self.sel = Some(DocSel { a, b: a, page });
                    self.sel_drag = true;
                }
            }
        }
        if self.sel_drag {
            if down {
                if let (Some(q), Some(s)) = (pos, self.sel.as_mut()) {
                    if let Some(b) = at(q, true) {
                        s.b = b;
                    }
                }
            } else {
                self.sel_drag = false;
            }
        }
        let sel = self.sel.filter(|s| s.page == page && s.a != s.b);
        // una selección vieja puede apuntar a trozos que ya no existen
        let range = sel.map(|s| if s.a <= s.b { (s.a, s.b) } else { (s.b, s.a) }).filter(|(a, _)| a.0 < runs.len());
        if let Some((a, b)) = range {
            for i in a.0..=b.0.min(runs.len().saturating_sub(1)) {
                let r = &runs[i];
                let n = r.text.chars().count().max(1) as f32;
                let (c0, c1) = (if i == a.0 { a.1 as f32 } else { 0.0 }, if i == b.0 { b.1 as f32 } else { n });
                p.rect_filled(to_screen(r.x + r.w * c0 / n, r.y, r.w * (c1 - c0).max(0.0) / n, r.h), CornerRadius::same(1), Color32::from_rgba_unmultiplied(60, 120, 255, 90));
            }
        }
        if keys && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) && !runs.is_empty() {
            let text = match range {
                Some((a, b)) => crate::docview::join_runs(runs[a.0..=b.0.min(runs.len() - 1)].iter(), Some((a.1, b.1))),
                None => crate::docview::join_runs(runs.iter(), None),
            };
            ctx.copy_text(text);
        }
        if self.sel_drag {
            ctx.set_cursor_icon(egui::CursorIcon::Text);
        } else if hot && pos.map(|q| pr.contains(q) && at(q, false).is_some()).unwrap_or(false) {
            ctx.set_cursor_icon(egui::CursorIcon::Text);
        }
    }

    /// Cuadro de búsqueda arriba del área; `info` = (actual, total, terminada). Devuelve el salto pedido (‹ ›).
    #[allow(clippy::too_many_arguments)]
    fn draw_find_bar(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, pal: &Pal, click: Option<Pos2>, info: (usize, usize, bool)) -> isize {
        let r = find_rect(area);
        p.rect_filled(r.translate(vec2(0.0, 2.0)), CornerRadius::same(10), Color32::from_black_alpha(60));
        p.rect_filled(r, CornerRadius::same(10), pal.panel);
        p.rect_stroke(r, CornerRadius::same(10), Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
        let blink = (ctx.input(|i| i.time) * 2.0) as i64 % 2 == 0;
        let hint = if self.doc.is_some() { t("Buscar en el documento") } else { t("Buscar") };
        let shown = if self.find_text.is_empty() { hint.to_string() } else { format!("{}{}", self.find_text, if blink { "|" } else { "" }) };
        let col = if self.find_text.is_empty() { pal.subtle } else { pal.text };
        p.with_clip_rect(Rect::from_min_max(r.min, pos2(r.right() - 130.0, r.bottom()))).text(pos2(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, shown, FontId::proportional(14.0), col);
        let (cur, total, done) = info;
        let info = if self.find_text.is_empty() { String::new() } else if total == 0 { if done { t("Sin resultados").to_string() } else { "…".into() } } else { format!("{} / {}{}", cur + 1, total, if done { "" } else { "+" }) };
        p.text(pos2(r.right() - 108.0, r.center().y), Align2::RIGHT_CENTER, info, FontId::proportional(12.0), pal.subtle);
        let mut step = 0isize;
        let mut close = false;
        for (k, (g, d)) in [("‹", -1isize), ("›", 1), ("✕", 0)].into_iter().enumerate() {
            let b = Rect::from_center_size(pos2(r.right() - 84.0 + k as f32 * 28.0 + 14.0, r.center().y), vec2(26.0, 26.0));
            if click.map(|q| b.contains(q)).unwrap_or(false) {
                if k == 2 { close = true } else { step = d }
            }
            p.text(b.center(), Align2::CENTER_CENTER, g, FontId::proportional(18.0), pal.text);
        }
        if close {
            self.close_find();
        }
        if self.find_open {
            // parpadeo del cursor
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        step
    }

    fn draw_zoomable(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, tex: egui::TextureId, sz: egui::Vec2, hot: bool) -> Rect {
        if hot {
            let pinch = ctx.input(|i| i.zoom_delta());
            if pinch != 1.0 {
                self.zoom = (self.zoom * pinch).clamp(0.25, 24.0);
            }
            let (dy, drag, middle, dbl) = ctx.input(|i| {
                (
                    i.smooth_scroll_delta.y,
                    if i.pointer.primary_down() && !edge_start(i) { i.pointer.delta() } else { vec2(0.0, 0.0) },
                    // botón central: mueve siempre (también sobre el texto de un documento)
                    if i.pointer.middle_down() { i.pointer.delta() } else { vec2(0.0, 0.0) },
                    i.pointer.button_double_clicked(egui::PointerButton::Primary),
                )
            });
            // en documentos la rueda cambia de página; el zoom va con Ctrl+rueda o pellizco
            if dy != 0.0 && self.doc.is_none() {
                self.zoom = (self.zoom * (dy * 0.0015).exp()).clamp(0.25, 24.0);
            }
            if !self.sel_drag {
                self.pan += drag;
            }
            self.pan += middle;
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
        let lim = vec2(((size.x - avail.width()) / 2.0).max(0.0), ((size.y - avail.height()) / 2.0).max(0.0));
        self.pan_lim = lim;
        self.pan = vec2(self.pan.x.clamp(-lim.x, lim.x), self.pan.y.clamp(-lim.y, lim.y));
        let rect = Rect::from_center_size(area.center() + self.pan, size);
        let shape = egui::epaint::RectShape::filled(rect, CornerRadius::same(8), Color32::WHITE)
            .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
        p.add(shape);
        rect
    }
}

/// Rectángulo del cuadro de búsqueda dentro de `area`.
fn find_rect(area: Rect) -> Rect {
    let w = 380.0f32.min(area.width() - 16.0);
    Rect::from_min_size(pos2(area.center().x - w / 2.0, area.top() + 10.0), vec2(w, 36.0))
}

/// Un toque que empieza pegado al borde de la pantalla es el gesto de «atrás» del sistema: no mueve el contenido.
pub(crate) fn edge_start(i: &egui::InputState) -> bool {
    if !(cfg!(target_os = "android") || i.any_touches()) {
        return false;
    }
    let sc = i.content_rect();
    i.pointer.press_origin().map(|o| o.x < sc.left() + 32.0 || o.x > sc.right() - 32.0).unwrap_or(false)
}

/// Visita cada sector del gráfico: (nodo, ruta relativa, ángulo inicial/final, anillo 1.., índice de color).
fn sectors(node: &crate::diskusage::Node, a0: f32, a1: f32, ring: usize, rings: usize, path: &mut Vec<usize>, hue: usize, f: &mut dyn FnMut(&crate::diskusage::Node, &[usize], f32, f32, usize, usize)) {
    if ring > rings || node.size == 0 {
        return;
    }
    let mut a = a0;
    for (i, k) in node.kids.iter().enumerate() {
        let span = (a1 - a0) * k.size as f32 / node.size as f32;
        if span > 0.01 {
            path.push(i);
            let h = if ring == 1 { i } else { hue };
            f(k, path, a, a + span, ring, h);
            sectors(k, a, a + span, ring + 1, rings, path, h, f);
            path.pop();
        }
        a += span;
    }
}

/// Lista de nombres alineada a la izquierda y repartida en las columnas que quepan (se llenan de arriba abajo).
/// Devuelve cuántos se han dibujado y el índice bajo el puntero, si lo hay y es seleccionable (no carpeta).
fn name_columns<'a>(cp: &egui::Painter, pal: &Pal, area: Rect, n: usize, get: impl Fn(usize) -> (&'a str, Option<u64>, bool), pointer: Option<Pos2>) -> (usize, Option<usize>) {
    const ROW: f32 = 22.0;
    let inner = Rect::from_min_max(pos2(area.left() + 8.0, area.top() + 2.0), pos2(area.right() - 8.0, area.bottom() - 4.0));
    let ncols = ((inner.width() / 250.0).floor() as usize).max(1);
    let col_w = inner.width() / ncols as f32;
    let per_col = ((inner.height() / ROW).floor() as usize).max(1);
    let shown = n.min(per_col * ncols);
    let mut hit = None;
    for i in 0..shown {
        let (name, size, dir) = get(i);
        let (c, r) = (i / per_col, i % per_col);
        let row = Rect::from_min_size(pos2(inner.left() + c as f32 * col_w, inner.top() + r as f32 * ROW), vec2(col_w - 6.0, ROW));
        if !dir && pointer.map(|q| row.contains(q)).unwrap_or(false) {
            cp.rect_filled(row, CornerRadius::same(6), pal.hover);
            hit = Some(i);
        }
        let size_txt = size.map(human_size);
        let maxw = col_w - 26.0 - if size_txt.is_some() { 64.0 } else { 0.0 };
        let font = FontId::proportional(13.0);
        let mut shown_name = name.to_string();
        let mut g = cp.layout_no_wrap(shown_name.clone(), font.clone(), pal.text);
        while g.size().x > maxw && shown_name.chars().count() > 4 {
            let keep = shown_name.chars().count() - (shown_name.chars().count() / 8).max(1);
            shown_name = shown_name.chars().take(keep).collect();
            g = cp.layout_no_wrap(format!("{shown_name}…"), font.clone(), pal.text);
        }
        let col = if dir { pal.accent } else { pal.text };
        cp.galley(pos2(row.left() + 8.0, row.center().y - g.size().y / 2.0), g, col);
        if let Some(sz) = size_txt {
            cp.text(pos2(row.right() - 6.0, row.center().y), Align2::RIGHT_CENTER, sz, FontId::proportional(11.5), pal.subtle);
        }
    }
    (shown, hit)
}

fn usage_ui(cp: &egui::Painter, pal: &Pal, ctx: &egui::Context, area: Rect, root_path: PathBuf, uv: &mut UsageView, pointer: Option<Pos2>, clicked: bool) {
    use std::f32::consts::{PI, TAU};
    use std::sync::atomic::Ordering as AO;
    if uv.root.is_none() && uv.scan.done.load(AO::Acquire) {
        uv.root = lock(&uv.scan.result).take().map(Arc::new);
    }
    let Some(root) = uv.root.clone() else {
        let msg = if uv.scan.done.load(AO::Relaxed) {
            t("Sin datos").to_string()
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            format!("{}  {}", t("Calculando…"), tf("{} archivos", &[&uv.scan.files.load(AO::Relaxed)]))
        };
        cp.text(area.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), pal.subtle);
        if !uv.scan.done.load(AO::Relaxed) {
            cp.text(
                area.center() + vec2(0.0, 24.0),
                Align2::CENTER_CENTER,
                human_size(uv.scan.bytes.load(AO::Relaxed)),
                FontId::proportional(13.0),
                pal.subtle,
            );
        }
        return;
    };
    let cur = root.at(&uv.path);
    let list_w = if area.width() > 560.0 { 230.0 } else { 0.0 };
    let chart = Rect::from_min_max(area.min, pos2(area.right() - list_w, area.bottom()));
    let c = chart.center();
    let big = (chart.width().min(chart.height()) / 2.0 - 10.0).max(40.0);
    let r0 = big * 0.24;
    let partial = uv.scan.partial.load(AO::Relaxed);
    // resultado parcial: menos anillos y aviso
    let rings = if partial { 3 } else { crate::diskusage::DEPTH };
    let th = (big - r0) / rings as f32;

    // sector bajo el puntero
    let mut hover: Option<(Vec<usize>, String, u64, bool)> = None;
    let mut centre_hot = false;
    if let Some(q) = pointer {
        let d = q - c;
        let r = d.length();
        if r <= r0 {
            centre_hot = chart.contains(q);
        } else if r <= big {
            let mut ang = d.y.atan2(d.x) + PI / 2.0;
            if ang < 0.0 {
                ang += TAU;
            }
            let ring = ((r - r0) / th) as usize + 1;
            sectors(cur, 0.0, TAU, 1, rings, &mut Vec::new(), 0, &mut |n, path, a0, a1, rg, _| {
                if rg == ring && ang >= a0 && ang < a1 {
                    hover = Some((path.to_vec(), n.name.clone(), n.size, n.dir && !n.kids.is_empty() && !n.other));
                }
            });
        }
    }
    let hp = hover.as_ref().map(|h| h.0.clone());

    let mut mesh = egui::epaint::Mesh::default();
    sectors(cur, 0.0, TAU, 1, rings, &mut Vec::new(), 0, &mut |n, path, a0, a1, rg, h| {
        let hot = hp.as_deref() == Some(path);
        let col: Color32 = if n.other {
            pal.border
        } else {
            let v = (0.86 - 0.06 * (rg - 1) as f32 + if hot { 0.12 } else { 0.0 }).clamp(0.0, 1.0);
            egui::ecolor::Hsva::new((h as f32 * 0.137) % 1.0, 0.55, v, 1.0).into()
        };
        let (ri, ro) = (r0 + (rg - 1) as f32 * th + 1.0, r0 + rg as f32 * th - 1.0);
        let n_seg = ((a1 - a0) / 0.06).ceil().max(1.0) as usize;
        let base = mesh.vertices.len() as u32;
        for k in 0..=n_seg {
            let a = a0 + (a1 - a0) * k as f32 / n_seg as f32 - PI / 2.0;
            let dir = vec2(a.cos(), a.sin());
            mesh.colored_vertex(c + dir * ri, col);
            mesh.colored_vertex(c + dir * ro, col);
        }
        for k in 0..n_seg as u32 {
            let i = base + 2 * k;
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i + 1, i + 3, i + 2);
        }
    });
    cp.add(egui::Shape::mesh(mesh));

    // centro: carpeta actual o sector señalado
    cp.circle_filled(c, r0 - 2.0, if centre_hot && !uv.path.is_empty() { pal.hover } else { pal.surface });
    let (name, size) = match &hover {
        Some((_, n, s, _)) => (n.clone(), *s),
        None => (cur.name.clone(), cur.size),
    };
    let mut nm = name;
    if nm.chars().count() > 16 {
        nm = nm.chars().take(15).collect::<String>() + "…";
    }
    cp.text(c + vec2(0.0, -9.0), Align2::CENTER_CENTER, nm, FontId::proportional(13.0), pal.text);
    let pct = if cur.size > 0 { format!("  ·  {:.0}%", size as f32 * 100.0 / cur.size as f32) } else { String::new() };
    let sub = if hover.is_some() { format!("{}{}", human_size(size), pct) } else { human_size(size) };
    cp.text(c + vec2(0.0, 9.0), Align2::CENTER_CENTER, sub, FontId::proportional(12.0), pal.subtle);

    let mut more = false;
    if partial {
        let r = Rect::from_min_size(pos2(chart.left() + 18.0, chart.bottom() - 30.0), vec2(150.0, 24.0));
        let hot = pointer.map(|q| r.contains(q)).unwrap_or(false);
        cp.text(pos2(r.left(), r.top() - 8.0), Align2::LEFT_CENTER, t("Tamaños aproximados (análisis parcial)"), FontId::proportional(11.5), pal.subtle);
        cp.rect_filled(r, CornerRadius::same(8), if hot { pal.hover } else { pal.surface });
        cp.text(r.center(), Align2::CENTER_CENTER, t("Seguir analizando"), FontId::proportional(12.5), pal.text);
        more = hot && clicked;
    }
    if more {
        uv.scan = crate::diskusage::start(root_path, ctx, false);
        uv.root = None;
        uv.path.clear();
        return;
    }
    if clicked {
        if let Some((p, _, _, true)) = &hover {
            uv.path.extend(p.iter().copied());
        } else if centre_hot && !uv.path.is_empty() {
            uv.path.pop();
        }
    }

    // lista de los mayores elementos
    if list_w > 0.0 {
        let x = area.right() - list_w + 8.0;
        let mut y = area.top() + 22.0;
        for (i, k) in cur.kids.iter().take(((area.height() - 20.0) / 24.0) as usize).enumerate() {
            let col: Color32 = if k.other { pal.border } else { egui::ecolor::Hsva::new((i as f32 * 0.137) % 1.0, 0.55, 0.86, 1.0).into() };
            cp.rect_filled(Rect::from_center_size(pos2(x + 5.0, y), vec2(10.0, 10.0)), CornerRadius::same(3), col);
            let mut nm = k.name.clone();
            if nm.chars().count() > 18 {
                nm = nm.chars().take(17).collect::<String>() + "…";
            }
            cp.text(pos2(x + 18.0, y), Align2::LEFT_CENTER, nm, FontId::proportional(12.5), pal.text);
            let pct = if cur.size > 0 { k.size as f32 * 100.0 / cur.size as f32 } else { 0.0 };
            cp.text(pos2(area.right() - 12.0, y), Align2::RIGHT_CENTER, format!("{}  {:.0}%", human_size(k.size), pct), FontId::proportional(12.0), pal.subtle);
            y += 24.0;
        }
    }
}


/// Contorno de lo seleccionado (pestaña o botón activo): el color de foco, con ≥ 3:1 sobre el fondo y
/// sobre el tinte de selección, para no depender sólo del relleno `pal.sel`.
fn sel_ring(pal: &Pal) -> Color32 {
    let on = [pal.bg, pal.panel, pal.surface, theme::over(pal.sel, pal.bg), theme::over(pal.sel, pal.surface)];
    theme::ensure_contrast_all(pal.focus, &on, theme::MIN_UI_CONTRAST + 0.03)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_arch = "wasm32")))]
    #[test]
    fn fifo_preview_does_not_block() {
        let dir = std::env::temp_dir().join(format!("fb-preview-fifo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let t0 = std::time::Instant::now();
        for name in ["a.md", "b.csv", "c.json", "d.sqlite", "e.txt", "f.png"] {
            let p = dir.join(name);
            let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            assert!(read_limited(&p, 1 << 20).is_none());
            assert!(matches!(text_raw(&p), Raw::Err(_)));
            let kind = crate::fsys::kind_of_ext(p.extension().unwrap().to_str().unwrap());
            assert!(matches!(load_guarded(&Job { generation: 1, path: p, kind }), Raw::Binary));
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(2));
        let ok = dir.join("ok.md");
        std::fs::write(&ok, "# hola").unwrap();
        assert_eq!(read_limited(&ok, 1 << 20).as_deref(), Some("# hola"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn preview_text_utf16_y_cola() {
        let dir = std::env::temp_dir().join(format!("fb-preview-text-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // UTF-16 LE con BOM: texto, no binario
        let p = dir.join("u16.txt");
        let mut b = vec![0xFF, 0xFE];
        b.extend("hola\nañil\n".encode_utf16().flat_map(|u| u.to_le_bytes()));
        std::fs::write(&p, b).unwrap();
        let Raw::Text(td) = text_raw(&p) else { panic!("UTF-16 no es texto") };
        assert_eq!(td.lines.first().map(|s| s.as_str()), Some("hola"));
        assert_eq!(td.lines.get(1).map(|s| s.as_str()), Some("añil"));
        // más grande que la cabeza: principio + final con hueco
        let p = dir.join("grande.txt");
        let mut s = String::new();
        let mut n = 0;
        while (s.len() as u64) < TEXT_HEAD + TEXT_TAIL * 2 {
            s.push_str(&format!("linea {n}\n"));
            n += 1;
        }
        std::fs::write(&p, &s).unwrap();
        let Raw::Text(td) = text_raw(&p) else { panic!("no es texto") };
        assert!(td.gap.is_some());
        assert_eq!(td.lines.first().map(|s| s.as_str()), Some("linea 0"));
        assert_eq!(td.lines.last().map(|s| s.as_str()), Some(format!("linea {}", n - 1).as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
