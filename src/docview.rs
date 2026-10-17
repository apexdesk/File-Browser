//! Documentos de oficina y PDF en la vista previa y las miniaturas (con la biblioteca `apex-view`).
//! Cada visor abre el archivo en un hilo propio y dibuja la página pedida; sólo se atiende la última petición.
use crate::thumbs::Decoded;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Máximo ancho (en píxeles) al que se dibuja una página.
const MAX_W: u32 = 2800;
/// Los documentos grandes no se abren para las miniaturas.
const THUMB_MAX_BYTES: u64 = 30 << 20;

pub fn is_doc_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "pdf" | "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "rtf" | "doc" | "xls" | "ppt"
    )
}

pub fn is_doc_path(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(is_doc_ext).unwrap_or(false)
}

fn err_text(e: &apex_view::Error) -> String {
    match e {
        apex_view::Error::Encrypted => crate::i18n::t("Documento protegido con contraseña").to_string(),
        apex_view::Error::TooLarge => crate::i18n::t("El documento es demasiado grande para mostrarlo").to_string(),
        _ => crate::i18n::t("No se pudo mostrar el documento").to_string(),
    }
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

enum Resp {
    Opened { count: usize },
    Failed(String),
    Page { id: u64, page: usize, width: u32, img: apex_view::Image },
    PageFailed { id: u64, msg: String },
}

/// Documento abierto en la vista previa.
pub struct DocView {
    tx: Sender<Req>,
    rx: Receiver<Resp>,
    latest: Arc<AtomicU64>,
    next_id: u64,
    pub count: usize,
    pub page: usize,
    pub error: Option<String>,
    opened: bool,
    tex: Option<(egui::TextureHandle, usize, u32)>,
    /// (página, ancho) que se está dibujando ahora
    pending: Option<(usize, u32)>,
}

impl DocView {
    pub fn new(ctx: &egui::Context, path: &PathBuf) -> DocView {
        let (tx, req_rx) = channel::<Req>();
        let (resp_tx, rx) = channel::<Resp>();
        let latest = Arc::new(AtomicU64::new(0));
        let (l, ctx2, p) = (latest.clone(), ctx.clone(), path.clone());
        std::thread::spawn(move || {
            let mut doc = match apex_view::Document::open_path(&p) {
                Ok(d) => d,
                Err(e) => {
                    let _ = resp_tx.send(Resp::Failed(err_text(&e)));
                    ctx2.request_repaint();
                    return;
                }
            };
            let _ = resp_tx.send(Resp::Opened { count: doc.page_count() });
            ctx2.request_repaint();
            while let Ok(mut req) = req_rx.recv() {
                // sólo importa la última petición
                while let Ok(r) = req_rx.try_recv() {
                    req = r;
                }
                let Req::Render { id, page, width } = req;
                let l2 = l.clone();
                let msg = match doc.render_page(page, width, &move || l2.load(Ordering::Relaxed) != id) {
                    Ok(img) => Resp::Page { id, page, width, img },
                    Err(apex_view::Error::Cancelled) => continue,
                    Err(e) => Resp::PageFailed { id, msg: err_text(&e) },
                };
                if resp_tx.send(msg).is_err() {
                    return;
                }
                ctx2.request_repaint();
            }
        });
        DocView { tx, rx, latest, next_id: 0, count: 0, page: 0, error: None, opened: false, tex: None, pending: None }
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
                    let ci = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba);
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

    pub fn busy(&self) -> bool {
        self.pending.is_some() || !self.opened && self.error.is_none()
    }
}
