//! Documentos paginados en la vista previa y las miniaturas: oficina y PDF (biblioteca `apex-view`)
//! y cómics (CBZ/CBR). Cada visor abre el archivo en un hilo propio y dibuja la página pedida; sólo se
//! atiende la última petición. Un segundo hilo da el texto de las páginas (búsqueda y selección).
use crate::thumbs::Decoded;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Máximo ancho (en píxeles) al que se dibuja una página.
const MAX_W: u32 = 2800;
/// Los documentos grandes no se abren para las miniaturas.
const THUMB_MAX_BYTES: u64 = 30 << 20;
/// Coincidencias que se guardan como máximo en una búsqueda.
const MAX_HITS: usize = 5000;

pub fn is_doc_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "pdf" | "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "rtf" | "doc" | "xls" | "ppt"
    )
}

pub fn is_doc_path(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(is_doc_ext).unwrap_or(false)
}

/// ¿Se abre con el visor de páginas (documento de oficina, PDF o cómic)?
pub fn is_paged_path(p: &Path) -> bool {
    is_doc_path(p) || p.extension().and_then(|e| e.to_str()).map(crate::comic::is_comic_ext).unwrap_or(false)
}

fn err_text(e: &apex_view::Error) -> String {
    match e {
        apex_view::Error::Encrypted => crate::i18n::t("Documento protegido con contraseña").to_string(),
        apex_view::Error::TooLarge => crate::i18n::t("El documento es demasiado grande para mostrarlo").to_string(),
        _ => crate::i18n::t("No se pudo mostrar el documento").to_string(),
    }
}

/// Texto de una página: caja como fracciones del ancho y el alto de la página.
#[derive(Clone)]
pub struct Run {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Fuente de páginas de un documento.
pub trait Pages {
    fn count(&self) -> usize;
    fn render(&mut self, page: usize, width: u32, cancel: &dyn Fn() -> bool) -> Result<Decoded, String>;
    fn runs(&mut self, _page: usize) -> Vec<Run> {
        Vec::new()
    }
}

type Opener = Arc<dyn Fn() -> Result<Box<dyn Pages>, String> + Send + Sync>;

struct ApexPages(apex_view::Document);

impl Pages for ApexPages {
    fn count(&self) -> usize {
        self.0.page_count()
    }

    fn render(&mut self, page: usize, width: u32, cancel: &dyn Fn() -> bool) -> Result<Decoded, String> {
        match self.0.render_page(page, width, cancel) {
            Ok(img) => Ok(Decoded { w: img.width as usize, h: img.height as usize, rgba: img.rgba }),
            Err(apex_view::Error::Cancelled) => Err(String::new()),
            Err(e) => Err(err_text(&e)),
        }
    }

    fn runs(&mut self, page: usize) -> Vec<Run> {
        self.0.page_runs(page).unwrap_or_default().into_iter().map(|r| Run { text: r.text, x: r.x, y: r.y, w: r.width, h: r.height }).collect()
    }
}

fn opener_for(path: &Path) -> Opener {
    let p = path.to_path_buf();
    if p.extension().and_then(|e| e.to_str()).map(crate::comic::is_comic_ext).unwrap_or(false) {
        return Arc::new(move || crate::comic::open(&p).map(|c| Box::new(c) as Box<dyn Pages>));
    }
    Arc::new(move || {
        apex_view::Document::open_path(&p).map(|d| Box::new(ApexPages(d)) as Box<dyn Pages>).map_err(|e| err_text(&e))
    })
}

/// Miniatura de la primera página.
pub fn thumb(path: &Path, max: u32) -> Option<Decoded> {
    if std::fs::metadata(path).ok()?.len() > THUMB_MAX_BYTES {
        return None;
    }
    let mut doc = apex_view::Document::open_path(path).ok()?;
    let (w, h) = doc.page_size(0).ok()?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let width = if h > w { (max as f32 * w / h).round().max(8.0) as u32 } else { max };
    let img = doc.render_page(0, width, &|| false).ok()?;
    Some(Decoded { w: img.width as usize, h: img.height as usize, rgba: img.rgba })
}

enum Req {
    Render { id: u64, page: usize, width: u32 },
}

enum TextReq {
    Runs { page: usize },
    Search { id: u64, query: String },
}

/// Coincidencia de una búsqueda: página y caja (fracciones de la página).
#[derive(Clone, Copy)]
pub struct Hit {
    pub page: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

enum Resp {
    Opened { count: usize },
    Failed(String),
    Page { id: u64, page: usize, width: u32, img: Decoded },
    PageFailed { id: u64, msg: String },
    Runs { page: usize, runs: Vec<Run> },
    Hits { id: u64, hits: Vec<Hit>, scanned: usize, done: bool },
}

/// Búsqueda de texto en curso o terminada.
pub struct Find {
    pub query: String,
    id: u64,
    pub hits: Vec<Hit>,
    pub cur: usize,
    pub scanned: usize,
    pub done: bool,
}

/// Documento abierto en la vista previa.
pub struct DocView {
    tx: Sender<Req>,
    text_tx: Option<Sender<TextReq>>,
    rx: Receiver<Resp>,
    resp_tx: Sender<Resp>,
    opener: Opener,
    ctx: egui::Context,
    latest: Arc<AtomicU64>,
    latest_find: Arc<AtomicU64>,
    next_id: u64,
    find_seq: u64,
    pub count: usize,
    pub page: usize,
    pub error: Option<String>,
    opened: bool,
    tex: Option<(egui::TextureHandle, usize, u32)>,
    /// (página, ancho) que se está dibujando ahora
    pending: Option<(usize, u32)>,
    /// texto de la página actual (se pide sólo cuando hace falta)
    runs: Option<(usize, Vec<Run>)>,
    runs_pending: Option<usize>,
    pub find: Option<Find>,
}

impl DocView {
    pub fn new(ctx: &egui::Context, path: &PathBuf) -> DocView {
        let opener = opener_for(path);
        let (tx, req_rx) = channel::<Req>();
        let (resp_tx, rx) = channel::<Resp>();
        let latest = Arc::new(AtomicU64::new(0));
        let (l, ctx2, op, rt) = (latest.clone(), ctx.clone(), opener.clone(), resp_tx.clone());
        std::thread::spawn(move || {
            let mut doc = match op() {
                Ok(d) => d,
                Err(m) => {
                    let _ = rt.send(Resp::Failed(m));
                    ctx2.request_repaint();
                    return;
                }
            };
            let _ = rt.send(Resp::Opened { count: doc.count() });
            ctx2.request_repaint();
            while let Ok(mut req) = req_rx.recv() {
                // sólo importa la última petición
                while let Ok(r) = req_rx.try_recv() {
                    req = r;
                }
                let Req::Render { id, page, width } = req;
                let l2 = l.clone();
                let msg = match doc.render(page, width, &move || l2.load(Ordering::Relaxed) != id) {
                    Ok(img) => Resp::Page { id, page, width, img },
                    Err(m) if m.is_empty() => continue,
                    Err(msg) => Resp::PageFailed { id, msg },
                };
                if rt.send(msg).is_err() {
                    return;
                }
                ctx2.request_repaint();
            }
        });
        DocView {
            tx,
            text_tx: None,
            rx,
            resp_tx,
            opener,
            ctx: ctx.clone(),
            latest,
            latest_find: Arc::new(AtomicU64::new(0)),
            next_id: 0,
            find_seq: 0,
            count: 0,
            page: 0,
            error: None,
            opened: false,
            tex: None,
            pending: None,
            runs: None,
            runs_pending: None,
            find: None,
        }
    }

    /// Hilo del texto: se crea la primera vez que se pide.
    fn text_sender(&mut self) -> &Sender<TextReq> {
        if self.text_tx.is_none() {
            let (tx, rx) = channel::<TextReq>();
            let (op, rt, ctx, lf) = (self.opener.clone(), self.resp_tx.clone(), self.ctx.clone(), self.latest_find.clone());
            std::thread::spawn(move || {
                let Ok(mut doc) = op() else { return };
                while let Ok(req) = rx.recv() {
                    match req {
                        TextReq::Runs { page } => {
                            let runs = doc.runs(page);
                            if rt.send(Resp::Runs { page, runs }).is_err() {
                                return;
                            }
                            ctx.request_repaint();
                        }
                        TextReq::Search { id, query } => {
                            let q = query.to_lowercase();
                            let n = doc.count();
                            let mut batch: Vec<Hit> = Vec::new();
                            let mut total = 0usize;
                            for page in 0..n {
                                if lf.load(Ordering::Relaxed) != id {
                                    break;
                                }
                                for r in doc.runs(page) {
                                    let low: Vec<char> = r.text.to_lowercase().chars().collect();
                                    let qc: Vec<char> = q.chars().collect();
                                    let len = low.len().max(1) as f32;
                                    let mut i = 0;
                                    while !qc.is_empty() && i + qc.len() <= low.len() {
                                        if low[i..i + qc.len()] == qc[..] && total < MAX_HITS {
                                            batch.push(Hit { page, x: r.x + r.w * i as f32 / len, y: r.y, w: r.w * qc.len() as f32 / len, h: r.h });
                                            total += 1;
                                            i += qc.len();
                                        } else {
                                            i += 1;
                                        }
                                    }
                                }
                                let last = page + 1 == n;
                                if !batch.is_empty() || last || page % 8 == 7 {
                                    let hits = std::mem::take(&mut batch);
                                    if rt.send(Resp::Hits { id, hits, scanned: page + 1, done: last }).is_err() {
                                        return;
                                    }
                                    ctx.request_repaint();
                                }
                            }
                            if n == 0 {
                                let _ = rt.send(Resp::Hits { id, hits: Vec::new(), scanned: 0, done: true });
                                ctx.request_repaint();
                            }
                        }
                    }
                }
            });
            self.text_tx = Some(tx);
        }
        self.text_tx.as_ref().unwrap()
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(r) = self.rx.try_recv() {
            match r {
                Resp::Opened { count } => {
                    self.count = count;
                    self.opened = true;
                }
                Resp::Failed(m) => self.error = Some(m),
                Resp::Page { id, page, width, img } => {
                    if id == self.next_id {
                        self.pending = None;
                    }
                    let ci = egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.rgba);
                    match &mut self.tex {
                        Some((t, p, w)) => {
                            t.set(ci, egui::TextureOptions::LINEAR);
                            *p = page;
                            *w = width;
                        }
                        None => self.tex = Some((ctx.load_texture("docpage", ci, egui::TextureOptions::LINEAR), page, width)),
                    }
                }
                Resp::PageFailed { id, msg } => {
                    if id == self.next_id {
                        self.pending = None;
                        self.error = Some(msg);
                    }
                }
                Resp::Runs { page, runs } => {
                    if self.runs_pending == Some(page) {
                        self.runs_pending = None;
                    }
                    self.runs = Some((page, runs));
                }
                Resp::Hits { id, hits, scanned, done } => {
                    if let Some(f) = self.find.as_mut().filter(|f| f.id == id) {
                        let first = f.hits.is_empty();
                        f.hits.extend(hits);
                        f.scanned = scanned;
                        f.done = done;
                        // la primera coincidencia a partir de la página actual
                        if first && !f.hits.is_empty() {
                            f.cur = f.hits.iter().position(|h| h.page >= self.page).unwrap_or(0);
                            let p = f.hits[f.cur].page;
                            self.page = p.min(self.count.saturating_sub(1));
                        }
                    }
                }
            }
        }
    }

    pub fn ready(&self) -> bool {
        self.opened
    }

    pub fn goto(&mut self, page: usize) {
        if self.count > 0 {
            self.page = page.min(self.count - 1);
        }
    }

    pub fn step(&mut self, d: isize) {
        self.goto((self.page as isize + d).clamp(0, self.count.max(1) as isize - 1) as usize);
    }

    /// Pide la página actual al ancho dado si aún no la tenemos (o no se está dibujando ya).
    pub fn want(&mut self, width_px: f32) {
        if !self.opened || self.count == 0 || self.error.is_some() {
            return;
        }
        let width = ((width_px / 200.0).ceil() as u32 * 200).clamp(400, MAX_W);
        let have = self.tex.as_ref().map(|(_, p, w)| *p == self.page && *w >= width).unwrap_or(false);
        if have || self.pending == Some((self.page, width)) {
            return;
        }
        self.next_id += 1;
        self.latest.store(self.next_id, Ordering::Relaxed);
        self.pending = Some((self.page, width));
        let _ = self.tx.send(Req::Render { id: self.next_id, page: self.page, width });
    }

    /// Textura de la última página dibujada (puede ser de otra página mientras se prepara la nueva).
    pub fn texture(&self) -> Option<&egui::TextureHandle> {
        self.tex.as_ref().map(|(t, _, _)| t)
    }

    /// Página a la que corresponde la textura actual.
    pub fn tex_page(&self) -> Option<usize> {
        self.tex.as_ref().map(|(_, p, _)| *p)
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.runs_pending.is_some() || !self.opened && self.error.is_none()
    }

    /// Texto de la página actual, si ya llegó (lo pide la primera vez).
    pub fn runs(&mut self) -> Option<&[Run]> {
        if !self.opened || self.count == 0 {
            return None;
        }
        let page = self.page;
        if self.runs.as_ref().map(|r| r.0) != Some(page) && self.runs_pending != Some(page) {
            self.runs_pending = Some(page);
            let _ = self.text_sender().send(TextReq::Runs { page });
        }
        self.runs.as_ref().filter(|r| r.0 == page).map(|r| &r.1[..])
    }

    /// Empieza (o reinicia) la búsqueda de `query` en todo el documento.
    pub fn search(&mut self, query: &str) {
        self.find_seq += 1;
        let id = self.find_seq;
        self.latest_find.store(id, Ordering::Relaxed);
        if query.trim().is_empty() {
            self.find = Some(Find { query: query.to_string(), id, hits: Vec::new(), cur: 0, scanned: 0, done: true });
            return;
        }
        self.find = Some(Find { query: query.to_string(), id, hits: Vec::new(), cur: 0, scanned: 0, done: false });
        let _ = self.text_sender().send(TextReq::Search { id, query: query.to_string() });
    }

    pub fn close_find(&mut self) {
        self.latest_find.store(0, Ordering::Relaxed);
        self.find = None;
    }

    /// Salta a la coincidencia siguiente (+1) o anterior (-1).
    pub fn next_hit(&mut self, d: isize) {
        let Some(f) = self.find.as_mut() else { return };
        if f.hits.is_empty() {
            return;
        }
        let n = f.hits.len() as isize;
        f.cur = (f.cur as isize + d).rem_euclid(n) as usize;
        let p = f.hits[f.cur].page;
        self.goto(p);
    }

    /// Texto completo de la página actual (para copiar).
    pub fn page_text(&mut self) -> String {
        let Some(runs) = self.runs() else { return String::new() };
        join_runs(runs.iter(), None)
    }
}

/// Une trozos de texto: espacio dentro de una línea, salto de línea al cambiar de línea.
/// `clip` recorta el primer y el último trozo (índices de carácter).
pub fn join_runs<'a>(runs: impl Iterator<Item = &'a Run>, clip: Option<(usize, usize)>) -> String {
    let rs: Vec<&Run> = runs.collect();
    let mut out = String::new();
    for (k, r) in rs.iter().enumerate() {
        let chars: Vec<char> = r.text.chars().collect();
        let (a, b) = match clip {
            Some((a, b)) => (if k == 0 { a.min(chars.len()) } else { 0 }, if k + 1 == rs.len() { b.min(chars.len()) } else { chars.len() }),
            None => (0, chars.len()),
        };
        if k > 0 {
            let p = rs[k - 1];
            out.push(if (r.y - p.y).abs() > r.h.max(p.h) * 0.5 { '\n' } else { ' ' });
        }
        out.extend(&chars[a.min(b)..b]);
    }
    out
}
