//! Terminal integrada: PTY (portable-pty) + emulador (vt100) dibujado con el painter de egui.
use crate::i18n::{t, tf};
use crate::theme::Pal;
use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Stroke};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct Terminal {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    alive: Arc<AtomicBool>,
    size: (u16, u16),
    scroll: f32,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
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
        let mut cmd = CommandBuilder::new(if shell.trim().is_empty() { default_shell() } else { shell.to_string() });
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 5000)));
        let alive = Arc::new(AtomicBool::new(true));
        {
            let (parser, alive, ctx) = (parser.clone(), alive.clone(), ctx.clone());
            std::thread::spawn(move || {
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
        Ok(Terminal { parser, writer, master: pair.master, alive, size: (rows, cols), scroll: 0.0, _child: child })
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) != self.size && rows > 1 && cols > 5 {
            self.size = (rows, cols);
            let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
        }
    }

    /// Traduce eventos de egui a bytes para el PTY.
    fn feed_events(&mut self, ctx: &egui::Context) {
        let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
        let app_cursor = self.parser.lock().unwrap().screen().application_cursor();
        let mut out: Vec<u8> = Vec::new();
        for e in events {
            match e {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Paste(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Copy => out.push(0x03),
                egui::Event::Cut => out.push(0x18),
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
            self.scroll = 0.0;
            self.parser.lock().unwrap().screen_mut().set_scrollback(0);
            self.send(&out);
        }
    }

    /// Dibuja el terminal en `rect`. `focused` indica si recibe teclado.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, pal: &Pal, focused: bool) {
        let ctx = ui.ctx().clone();
        let font = FontId::monospace(13.0);
        let p = ui.painter_at(rect);
        let cw = p.layout_no_wrap("MMMMMMMMMM".into(), font.clone(), Color32::WHITE).size().x / 10.0;
        let ch = 18.0;
        let inner = rect.shrink2(vec2(10.0, 6.0));
        let cols = (inner.width() / cw).floor().max(10.0) as u16;
        let rows = (inner.height() / ch).floor().max(2.0) as u16;
        self.resize(rows, cols);

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
