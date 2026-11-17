//! Terminal integrada: PTY (portable-pty) + emulador (vt100) dibujado con el painter de egui.
#[allow(unused_imports)]
use crate::vfs::{KindV as _, PathV as _};
use crate::i18n::{t, tf};
use crate::theme::Pal;
use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Stroke};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

type Child = Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>;

pub struct Terminal {
    parser: Arc<Mutex<vt100::Parser>>,
    /// lo que se escribe va a un hilo propio: si el programa no lee su entrada, la interfaz no se congela
    writer: SyncSender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    alive: Arc<AtomicBool>,
    /// avisa al hilo que vigila el proceso de que el terminal se ha cerrado
    closed: Arc<AtomicBool>,
    size: (u16, u16),
    scroll: f32,
    child: Child,
    /// selección con el ratón: (fila, columna) de inicio y fin en la pantalla visible
    sel: Option<((u16, u16), (u16, u16))>,
}

/// Hilo que escribe en el PTY lo que le llega por un canal acotado. Si el canal está lleno (el
/// programa lleva rato sin leer), lo nuevo se descarta en lugar de bloquear a quien envía.
fn spawn_writer(mut w: Box<dyn Write + Send>) -> SyncSender<Vec<u8>> {
    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    crate::vfs::thread::spawn(move || {
        for b in rx {
            if w.write_all(&b).and_then(|_| w.flush()).is_err() {
                break;
            }
        }
    });
    tx
}

pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").map(|_| "powershell.exe".to_string()).unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
}

impl Terminal {
    pub fn spawn(ctx: &egui::Context, cwd: &Path, shell: &str) -> Result<Terminal, String> {
        let (rows, cols) = (14u16, 100u16);
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let shell = if shell.trim().is_empty() { default_shell() } else { shell.to_string() };
        // en Flatpak el shell (el $SHELL del anfitrión) y sus programas están fuera: se lanza en el anfitrión
        let mut cmd = if crate::systerm::in_flatpak() {
            let mut c = CommandBuilder::new("flatpak-spawn");
            c.arg("--host");
            c.arg("--watch-bus");
            let mut d = std::ffi::OsString::from("--directory=");
            d.push(cwd);
            c.arg(d);
            c.arg("--env=TERM=xterm-256color");
            c.arg("--env=COLORTERM=truecolor");
            c.arg(&shell);
            c
        } else {
            CommandBuilder::new(&shell)
        };
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = spawn_writer(pair.master.take_writer().map_err(|e| e.to_string())?);
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 5000)));
        let alive = Arc::new(AtomicBool::new(true));
        {
            let (parser, alive, ctx) = (parser.clone(), alive.clone(), ctx.clone());
            crate::vfs::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            parser.lock().unwrap().process(&buf[..n]);
                            ctx.request_repaint();
                        }
                    }
                }
                alive.store(false, Ordering::Release);
                ctx.request_repaint();
            });
        }
        // En Windows (ConPTY) la lectura no acaba cuando el proceso termina: se vigila el proceso
        let child: Child = Arc::new(Mutex::new(child));
        let closed = Arc::new(AtomicBool::new(false));
        {
            let (child, alive, closed, ctx) = (child.clone(), alive.clone(), closed.clone(), ctx.clone());
            crate::vfs::thread::spawn(move || {
                while !closed.load(Ordering::Acquire) && alive.load(Ordering::Acquire) {
                    if !matches!(child.lock().unwrap().try_wait(), Ok(None)) {
                        alive.store(false, Ordering::Release);
                        ctx.request_repaint();
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            });
        }
        Ok(Terminal { parser, writer, master: pair.master, alive, closed, size: (rows, cols), scroll: 0.0, child, sel: None })
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn send(&mut self, bytes: &[u8]) {
        match self.writer.try_send(bytes.to_vec()) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => self.alive.store(false, Ordering::Release),
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) != self.size && rows > 1 && cols > 5 {
            self.size = (rows, cols);
            let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
        }
    }

    /// Texto seleccionado con el ratón (vacío si no hay selección).
    fn selected_text(&self) -> String {
        let Some((a, b)) = self.sel else { return String::new() };
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        self.parser.lock().unwrap().screen().contents_between(s.0, s.1, e.0, e.1.saturating_add(1))
    }

    /// Traduce eventos de egui a bytes para el PTY.
    fn feed_events(&mut self, ctx: &egui::Context) {
        let (events, mods): (Vec<egui::Event>, egui::Modifiers) = ctx.input(|i| (i.events.clone(), i.modifiers));
        let app_cursor = self.parser.lock().unwrap().screen().application_cursor();
        let mac = cfg!(target_os = "macos");
        let mut out: Vec<u8> = Vec::new();
        for e in events {
            match e {
                // Alt+tecla: ESC delante, como los terminales (en macOS Opción escribe caracteres)
                egui::Event::Text(t) => {
                    if mods.alt && !mods.ctrl && !mods.command && !mac {
                        out.push(0x1b);
                    }
                    out.extend_from_slice(t.as_bytes())
                }
                egui::Event::Paste(t) => {
                    // con «bracketed paste» el shell no ejecuta lo pegado hasta que el usuario pulse Intro
                    let bp = self.parser.lock().unwrap().screen().bracketed_paste();
                    if bp {
                        out.extend_from_slice(b"\x1b[200~");
                    }
                    // sin ESC ni controles: un «\x1b[201~» pegado cerraría el modo y lo que sigue se ejecutaría
                    let clean: String = t.replace("\r\n", "\n").chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
                    out.extend_from_slice(clean.as_bytes());
                    if bp {
                        out.extend_from_slice(b"\x1b[201~");
                    }
                }
                // Ctrl+Mayús+C (⌘C en macOS) copia la selección; Ctrl+C sigue siendo ^C
                egui::Event::Copy => {
                    if mods.shift || mac {
                        let s = self.selected_text();
                        if !s.is_empty() {
                            ctx.copy_text(s);
                        }
                    } else {
                        out.push(0x03);
                    }
                }
                egui::Event::Cut => {
                    if !mods.shift && !mac {
                        out.push(0x18);
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    use egui::Key::*;
                    let arrow = |c: u8| -> Vec<u8> {
                        if modifiers.ctrl {
                            format!("\x1b[1;5{}", c as char).into_bytes()
                        } else if app_cursor {
                            vec![0x1b, b'O', c]
                        } else {
                            vec![0x1b, b'[', c]
                        }
                    };
                    let seq: Option<Vec<u8>> = match key {
                        Enter => Some(b"\r".to_vec()),
                        Tab => Some(if modifiers.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
                        Backspace => Some(vec![0x7f]),
                        Escape => Some(vec![0x1b]),
                        ArrowUp => Some(arrow(b'A')),
                        ArrowDown => Some(arrow(b'B')),
                        ArrowRight => Some(arrow(b'C')),
                        ArrowLeft => Some(arrow(b'D')),
                        Home => Some(b"\x1b[H".to_vec()),
                        End => Some(b"\x1b[F".to_vec()),
                        PageUp => Some(b"\x1b[5~".to_vec()),
                        PageDown => Some(b"\x1b[6~".to_vec()),
                        Delete => Some(b"\x1b[3~".to_vec()),
                        Insert => Some(b"\x1b[2~".to_vec()),
                        F1 => Some(b"\x1bOP".to_vec()),
                        F2 => Some(b"\x1bOQ".to_vec()),
                        F3 => Some(b"\x1bOR".to_vec()),
                        F4 => Some(b"\x1bOS".to_vec()),
                        F5 => Some(b"\x1b[15~".to_vec()),
                        F6 => Some(b"\x1b[17~".to_vec()),
                        F7 => Some(b"\x1b[18~".to_vec()),
                        F8 => Some(b"\x1b[19~".to_vec()),
                        F9 => Some(b"\x1b[20~".to_vec()),
                        F10 => Some(b"\x1b[21~".to_vec()),
                        F11 => Some(b"\x1b[23~".to_vec()),
                        F12 => Some(b"\x1b[24~".to_vec()),
                        _ => {
                            if modifiers.ctrl && !modifiers.alt {
                                let n = key.name();
                                if n.len() == 1 {
                                    let c = n.as_bytes()[0].to_ascii_uppercase();
                                    if c.is_ascii_uppercase() {
                                        Some(vec![c - b'A' + 1])
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                    };
                    if let Some(s) = seq {
                        out.extend_from_slice(&s);
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            self.sel = None;
            self.scroll = 0.0;
            self.parser.lock().unwrap().screen_mut().set_scrollback(0);
            self.send(&out);
        }
    }

    /// Dibuja el terminal en `rect`. `focused` indica si recibe teclado; `resp` es el área (arrastrar
    /// selecciona texto).
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, resp: &egui::Response, pal: &Pal, focused: bool) {
        let ctx = ui.ctx().clone();
        let font = FontId::monospace(13.0);
        let p = ui.painter_at(rect);
        let cw = p.layout_no_wrap("MMMMMMMMMM".into(), font.clone(), Color32::WHITE).size().x / 10.0;
        let ch = 18.0;
        let inner = rect.shrink2(vec2(10.0, 6.0));
        let cols = (inner.width() / cw).floor().max(10.0) as u16;
        let rows = (inner.height() / ch).floor().max(2.0) as u16;
        self.resize(rows, cols);

        // selección con el ratón (celda bajo el puntero)
        let cell_at = |p: egui::Pos2| -> (u16, u16) {
            let r = ((p.y - inner.top()) / ch).floor().clamp(0.0, rows.saturating_sub(1) as f32) as u16;
            let c = ((p.x - inner.left()) / cw).floor().clamp(0.0, cols.saturating_sub(1) as f32) as u16;
            (r, c)
        };
        if resp.drag_started() {
            self.sel = resp.interact_pointer_pos().map(|p| (cell_at(p), cell_at(p)));
        } else if resp.dragged() {
            if let (Some(p), Some((a, _))) = (resp.interact_pointer_pos(), self.sel) {
                self.sel = Some((a, cell_at(p)));
            }
        } else if resp.clicked() {
            self.sel = None;
        }
        if resp.drag_stopped() && self.sel.is_some_and(|(a, b)| a == b) {
            self.sel = None;
        }

        if focused {
            self.feed_events(&ctx);
        }
        // scroll de historial con la rueda
        if ui.rect_contains_pointer(rect) {
            let dy = ctx.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                self.scroll += dy / ch;
                let n = self.scroll.trunc() as isize;
                if n != 0 {
                    self.scroll -= n as f32;
                    let mut pr = self.parser.lock().unwrap();
                    let cur = pr.screen().scrollback() as isize;
                    pr.screen_mut().set_scrollback((cur + n).max(0) as usize);
                }
            }
        }

        let bg = if pal.bg.r() < 128 { Color32::from_rgb(14, 15, 19) } else { Color32::from_rgb(250, 250, 252) };
        let fg_default = if pal.bg.r() < 128 { Color32::from_rgb(214, 218, 228) } else { Color32::from_rgb(40, 44, 54) };
        p.rect_filled(rect, CornerRadius::same(12), bg);

        if let Some((a, b)) = self.sel {
            let (s, e) = if a <= b { (a, b) } else { (b, a) };
            for r in s.0..=e.0.min(rows.saturating_sub(1)) {
                let c0 = if r == s.0 { s.1 } else { 0 };
                let c1 = if r == e.0 { e.1 + 1 } else { cols };
                let x0 = inner.left() + c0 as f32 * cw;
                let y0 = inner.top() + r as f32 * ch;
                p.rect_filled(Rect::from_min_max(pos2(x0, y0), pos2(inner.left() + c1 as f32 * cw, y0 + ch)), 0.0, pal.sel);
            }
        }
        let pr = self.parser.lock().unwrap();
        let screen = pr.screen();
        let (srow, scol) = screen.size();
        for r in 0..srow.min(rows) {
            let y = inner.top() + r as f32 * ch;
            let mut run = String::new();
            let mut run_col = 0u16;
            let mut run_style: Option<(Color32, bool)> = None;
            let flush = |p: &egui::Painter, run: &mut String, col: u16, st: Option<(Color32, bool)>| {
                if run.trim().is_empty() && st.map(|s| !s.1).unwrap_or(true) {
                    run.clear();
                    return;
                }
                if let Some((c, bold)) = st {
                    let x = inner.left() + col as f32 * cw;
                    let f = if bold { FontId::monospace(13.0) } else { FontId::monospace(13.0) };
                    p.text(pos2(x, y), Align2::LEFT_TOP, run.as_str(), f, c);
                    if bold {
                        p.text(pos2(x + 0.6, y), Align2::LEFT_TOP, run.as_str(), FontId::monospace(13.0), c);
                    }
                }
                run.clear();
            };
            for c in 0..scol.min(cols) {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let (mut f, mut b) = (conv(cell.fgcolor(), fg_default, false), conv(cell.bgcolor(), bg, true));
                if cell.inverse() {
                    std::mem::swap(&mut f, &mut b);
                }
                if b != bg {
                    let w = if cell.is_wide() { 2.0 } else { 1.0 };
                    p.rect_filled(
                        Rect::from_min_size(pos2(inner.left() + c as f32 * cw, y), vec2(cw * w, ch)),
                        0.0,
                        b,
                    );
                }
                let style = (f, cell.bold());
                let txt = if cell.has_contents() { cell.contents() } else { " " };
                if run_style != Some(style) {
                    flush(&p, &mut run, run_col, run_style);
                    run_col = c;
                    run_style = Some(style);
                }
                run.push_str(txt);
            }
            flush(&p, &mut run, run_col, run_style);
        }
        if screen.scrollback() == 0 && !screen.hide_cursor() {
            let (cr, cc) = screen.cursor_position();
            let r = Rect::from_min_size(pos2(inner.left() + cc as f32 * cw, inner.top() + cr as f32 * ch), vec2(cw, ch));
            if focused {
                p.rect_filled(r, 2.0, pal.accent.gamma_multiply(0.85));
            } else {
                p.rect_stroke(r, 2.0, Stroke::new(1.0, pal.subtle), egui::StrokeKind::Inside);
            }
        }
        if screen.scrollback() > 0 {
            p.text(
                pos2(rect.right() - 12.0, rect.top() + 8.0),
                Align2::RIGHT_TOP,
                tf("^ {} líneas", &[&screen.scrollback()]),
                FontId::proportional(11.0),
                pal.subtle,
            );
        }
        drop(pr);
        if !self.alive() {
            p.text(
                rect.center_bottom() - vec2(0.0, 14.0),
                Align2::CENTER_CENTER,
                t("[proceso terminado — cierra y vuelve a abrir el terminal]"),
                FontId::proportional(12.0),
                pal.subtle,
            );
        }
    }
}

impl Drop for Terminal {
    /// Al cerrar el terminal se termina el proceso y se recoge (sin zombis), en otro hilo para no
    /// esperar en la interfaz.
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let child = self.child.clone();
        crate::vfs::thread::spawn(move || {
            let mut c = child.lock().unwrap();
            if matches!(c.try_wait(), Ok(None)) {
                let _ = c.kill();
            }
            let _ = c.wait();
        });
    }
}

fn conv(c: vt100::Color, default: Color32, is_bg: bool) -> Color32 {
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188), (17, 168, 205), (204, 204, 204),
        (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255),
    ];
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i) => {
            let i = i as usize;
            let (r, g, b) = if i < 16 {
                ANSI[i]
            } else if i < 232 {
                let n = i - 16;
                let lv = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
                (lv(n / 36), lv((n / 6) % 6), lv(n % 6))
            } else {
                let g = (8 + (i - 232) * 10) as u8;
                (g, g, g)
            };
            let _ = is_bg;
            Color32::from_rgb(r, g, b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escritor que no acepta nada hasta que se abre la compuerta (un programa que no lee).
    struct Slow(Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>);
    impl Write for Slow {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            while !self.1.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_never_blocks() {
        let (out, open) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
        let tx = spawn_writer(Box::new(Slow(out.clone(), open.clone())));
        let t0 = std::time::Instant::now();
        let mut accepted = Vec::new();
        for i in 0..1000u32 {
            if tx.try_send(i.to_le_bytes().to_vec()).is_ok() {
                accepted.push(i);
            }
        }
        let sent = accepted.len();
        // con el escritor atascado, enviar no espera: lo que no cabe se descarta
        assert!(t0.elapsed() < std::time::Duration::from_secs(1));
        assert!((64..=66).contains(&sent), "{sent}");
        open.store(true, Ordering::Release);
        drop(tx);
        let t0 = std::time::Instant::now();
        while out.lock().unwrap().len() < sent * 4 && t0.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let got = out.lock().unwrap().clone();
        assert_eq!(got.len(), sent * 4);
        // en orden
        let first: Vec<u32> = got.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        assert_eq!(first, accepted);
    }
}
