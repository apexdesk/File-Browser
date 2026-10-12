//! Reproductor integrado. `ffmpeg -re` decodifica a PPM por tubería (ritmo de tiempo real) y se
//! dibuja como textura; el audio lo reproduce `ffplay` sincronizado al arrancar. Sin ffmpeg no hay
//! reproducción (la vista previa sigue mostrando el fotograma).
use egui::ColorImage;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

fn which(bin: &str) -> bool {
    let exe = if cfg!(windows) { format!("{bin}.exe") } else { bin.to_string() };
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(&exe).is_file())).unwrap_or(false)
}

pub fn have_ffmpeg() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| which("ffmpeg"))
}

fn have_ffplay() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| which("ffplay"))
}

fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

#[derive(Default)]
struct Shared {
    epoch: AtomicU64,
    ended: AtomicU64,
    frame: Mutex<Option<ColorImage>>,
}

pub struct Player {
    path: PathBuf,
    ctx: egui::Context,
    pub duration: f64,
    shared: Arc<Shared>,
    video: Option<Child>,
    audio: Option<Child>,
    base: f64,
    started: Option<Instant>,
    pub tex: Option<egui::TextureHandle>,
    pub has_audio: bool,
    pub error: Option<String>,
}

/// Duración en segundos leída de la cabecera de `ffmpeg -i`.
fn probe_duration(path: &Path) -> f64 {
    let mut c = Command::new("ffmpeg");
    quiet(&mut c).args(["-hide_banner", "-i"]).arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let Ok(out) = c.output() else { return 0.0 };
    let s = String::from_utf8_lossy(&out.stderr);
    let Some(i) = s.find("Duration: ") else { return 0.0 };
    let t: String = s[i + 10..].chars().take_while(|c| c.is_ascii_digit() || *c == ':' || *c == '.').collect();
    let mut secs = 0.0;
    for part in t.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().unwrap_or(0.0);
    }
    secs
}

impl Player {
    pub fn new(ctx: &egui::Context, path: &Path) -> Player {
        Player {
            path: path.to_path_buf(),
            ctx: ctx.clone(),
            duration: probe_duration(path),
            shared: Arc::new(Shared::default()),
            video: None,
            audio: None,
            base: 0.0,
            started: None,
            tex: None,
            has_audio: have_ffplay(),
            error: None,
        }
    }

    pub fn playing(&self) -> bool {
        self.started.is_some()
    }

    pub fn position(&self) -> f64 {
        let p = self.base + self.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
        if self.duration > 0.0 { p.min(self.duration) } else { p }
    }

    fn stop_procs(&mut self) {
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        for c in [self.video.take(), self.audio.take()].into_iter().flatten() {
            let mut c = c;
            let _ = c.kill();
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }

    fn start(&mut self, pos: f64) {
        self.stop_procs();
        let g = self.shared.epoch.load(Ordering::SeqCst);
        let mut v = Command::new("ffmpeg");
        quiet(&mut v)
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{pos:.3}"), "-re", "-i"])
            .arg(&self.path)
            .args(["-an", "-sn", "-vf", "scale='min(1280,iw)':-2:flags=fast_bilinear,fps=30", "-f", "image2pipe", "-vcodec", "ppm", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = match v.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let Some(out) = child.stdout.take() else { return };
        self.video = Some(child);
        let (shared, ctx) = (self.shared.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let mut r = BufReader::with_capacity(1 << 20, out);
            let mut line = String::new();
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if !line.starts_with("P6") {
                    break;
                }
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let mut it = line.split_whitespace().map(|x| x.parse::<usize>().unwrap_or(0));
                let (w, h) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
                line.clear();
                let _ = r.read_line(&mut line); // maxval
                if w == 0 || h == 0 || w > 8192 || h > 8192 {
                    break;
                }
                let mut buf = vec![0u8; w * h * 3];
                if r.read_exact(&mut buf).is_err() {
                    break;
                }
                if shared.epoch.load(Ordering::SeqCst) != g {
                    return;
                }
                *shared.frame.lock().unwrap() = Some(ColorImage::from_rgb([w, h], &buf));
                ctx.request_repaint();
            }
            if shared.epoch.load(Ordering::SeqCst) == g {
                shared.ended.store(g, Ordering::SeqCst);
            }
            ctx.request_repaint();
        });
        if self.has_audio {
            let mut a = Command::new("ffplay");
            quiet(&mut a)
                .args(["-nodisp", "-vn", "-autoexit", "-loglevel", "quiet", "-ss", &format!("{pos:.3}"), "-i"])
                .arg(&self.path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            self.audio = a.spawn().ok();
        }
        self.base = pos;
        self.started = Some(Instant::now());
    }

    pub fn toggle(&mut self) {
        if self.playing() {
            self.base = self.position();
            self.started = None;
            self.stop_procs();
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

    /// Sube el último fotograma recibido y detecta el final del vídeo.
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
            self.base = if self.duration > 0.0 { self.duration } else { self.position() };
            self.started = None;
            self.stop_procs();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop_procs();
    }
}
