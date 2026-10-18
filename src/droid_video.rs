//! Reproductor de vídeo en Android: decodifica con los códecs del sistema (`droid`) en un hilo y
//! dibuja el último fotograma como textura. Misma interfaz que el reproductor de escritorio.
use egui::ColorImage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// En Android el sistema ya trae los códecs: siempre hay reproducción (si el formato no se admite,
/// el reproductor lo dice al intentarlo y queda «Abrir con…»).
pub fn have_ffmpeg() -> bool {
    true
}

#[derive(Default)]
struct Shared {
    epoch: AtomicU64,
    ended: AtomicU64,
    frame: Mutex<Option<ColorImage>>,
    error: Mutex<Option<String>>,
}

pub struct Player {
    path: PathBuf,
    ctx: egui::Context,
    pub duration: f64,
    shared: Arc<Shared>,
    base: f64,
    started: Option<Instant>,
    pub tex: Option<egui::TextureHandle>,
    pub has_audio: bool,
    pub error: Option<String>,
}

impl Player {
    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn new(ctx: &egui::Context, path: &Path) -> Player {
        let info = crate::droid::probe(path);
        Player {
            path: path.to_path_buf(),
            ctx: ctx.clone(),
            duration: info.as_ref().map(|i| i.duration).unwrap_or(0.0),
            shared: Arc::new(Shared::default()),
            base: 0.0,
            started: None,
            tex: None,
            has_audio: info.as_ref().map(|i| i.has_audio).unwrap_or(false),
            error: if info.is_none() { Some("formato no admitido por el sistema".into()) } else { None },
        }
    }

    pub fn playing(&self) -> bool {
        self.started.is_some()
    }

    pub fn position(&self) -> f64 {
        let p = self.base + self.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
        if self.duration > 0.0 { p.min(self.duration) } else { p }
    }

    fn stop(&mut self) {
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn start(&mut self, pos: f64) {
        self.stop();
        self.error = None;
        let g = self.shared.epoch.load(Ordering::SeqCst);
        let (shared, ctx, path) = (self.shared.clone(), self.ctx.clone(), self.path.clone());
        std::thread::spawn(move || {
            let alive = || shared.epoch.load(Ordering::SeqCst) == g;
            let mut o = crate::droid::Opts { audio: true, realtime: true, max_w: 1280, bytes: false };
            let mut on_frame = |w: usize, h: usize, px: Vec<u8>| {
                if !alive() {
                    return false;
                }
                *shared.frame.lock().unwrap() = Some(ColorImage::from_rgba_unmultiplied([w, h], &px));
                ctx.request_repaint();
                true
            };
            let mut res = crate::droid::run(&path, (pos * 1e6) as i64, &o, &alive, &mut on_frame);
            if matches!(&res, Err(e) if e == crate::droid::NO_SURFACE) && alive() {
                // la superficie no entrega imágenes en este móvil: se repite leyendo los búferes del códec
                o.bytes = true;
                res = crate::droid::run(&path, (pos * 1e6) as i64, &o, &alive, &mut on_frame);
            }
            match res {
                Ok(_) => {}
                Err(e) => *shared.error.lock().unwrap() = Some(e),
            }
            if alive() {
                shared.ended.store(g, Ordering::SeqCst);
            }
            ctx.request_repaint();
        });
        self.base = pos;
        self.started = Some(Instant::now());
    }

    /// Tras volver de otra ventana: reinicia en la posición actual.
    pub fn resync(&mut self) {
        if self.playing() {
            let p = self.position();
            self.seek_to(p);
        }
    }

    pub fn toggle(&mut self) {
        if self.playing() {
            self.base = self.position();
            self.started = None;
            self.stop();
        } else {
            let at = if self.duration > 0.0 && self.base >= self.duration - 0.2 { 0.0 } else { self.base };
            self.start(at);
        }
    }

    pub fn seek_to(&mut self, secs: f64) {
        let max = if self.duration > 0.5 { self.duration - 0.3 } else { f64::MAX };
        self.start(secs.clamp(0.0, max));
    }

    pub fn seek_by(&mut self, d: f64) {
        let p = self.position();
        self.seek_to(p + d);
    }

    /// Sube el último fotograma recibido y detecta el final del vídeo o un error.
    pub fn poll(&mut self) {
        let f = self.shared.frame.lock().unwrap().take();
        if let Some(img) = f {
            match &mut self.tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => self.tex = Some(self.ctx.load_texture("video", img, egui::TextureOptions::LINEAR)),
            }
        }
        let g = self.shared.epoch.load(Ordering::SeqCst);
        if self.started.is_some() && self.shared.ended.load(Ordering::SeqCst) == g {
            if let Some(e) = self.shared.error.lock().unwrap().take() {
                self.error = Some(e);
                self.base = self.position();
            } else {
                self.base = if self.duration > 0.0 { self.duration } else { self.position() };
            }
            self.started = None;
            self.stop();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}
