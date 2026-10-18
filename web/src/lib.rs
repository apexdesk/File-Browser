//! Cliente web del File Browser: egui en WebAssembly sobre la API de filebrowser-server.

use serde::Deserialize;
use std::sync::{Arc, Mutex};

#[derive(Deserialize, Clone, Debug)]
pub struct Entry {
    pub n: String,
    pub d: bool,
    pub s: u64,
    pub m: i64,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Listing {
    pub path: Vec<String>,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub truncated: bool,
}

type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

pub struct App {
    api: String,
    cur: Listing,
    pending: Slot<Listing>,
    loading: bool,
    err: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, api: String) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let mut a = App { api, cur: Listing::default(), pending: Arc::default(), loading: false, err: None };
        a.go(Vec::new(), &cc.egui_ctx);
        a
    }

    fn go(&mut self, path: Vec<String>, ctx: &egui::Context) {
        self.loading = true;
        let slot = self.pending.clone();
        let ctx = ctx.clone();
        let url = format!("{}list?p={}", self.api, enc(&path.join("/")));
        ehttp::fetch(ehttp::Request::get(url), move |r| {
            let v = match r {
                Ok(resp) if resp.ok => serde_json::from_slice::<Listing>(&resp.bytes).map_err(|e| e.to_string()),
                Ok(resp) => Err(format!("{} {}", resp.status, resp.status_text)),
                Err(e) => Err(e),
            };
            if let Ok(mut g) = slot.lock() {
                *g = Some(v);
            }
            ctx.request_repaint();
        });
    }
}

fn enc(s: &str) -> String {
    let mut o = String::new();
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            o.push(c as char);
        } else {
            o.push_str(&format!("%{c:02X}"));
        }
    }
    o
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let done = self.pending.lock().ok().and_then(|mut g| g.take());
        if let Some(r) = done {
            self.loading = false;
            match r {
                Ok(l) => {
                    self.cur = l;
                    self.err = None;
                }
                Err(e) => self.err = Some(e),
            }
        }
        let mut nav: Option<Vec<String>> = None;
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.link("Inicio").clicked() {
                    nav = Some(Vec::new());
                }
                for i in 0..self.cur.path.len() {
                    ui.label("/");
                    if ui.link(&self.cur.path[i]).clicked() {
                        nav = Some(self.cur.path[..=i].to_vec());
                    }
                }
                if self.loading {
                    ui.spinner();
                }
            });
            if let Some(e) = &self.err {
                ui.colored_label(egui::Color32::LIGHT_RED, e);
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for e in &self.cur.entries {
                    let label = if e.d { format!("📁 {}", e.n) } else { e.n.clone() };
                    if ui.selectable_label(false, label).clicked() && e.d {
                        let mut p = self.cur.path.clone();
                        p.push(e.n.clone());
                        nav = Some(p);
                    }
                }
            });
        });
        if let Some(p) = nav {
            let ctx = ui.ctx().clone();
            self.go(p, &ctx);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod boot {
    use super::App;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    #[wasm_bindgen(start)]
    pub fn start() {
        wasm_bindgen_futures::spawn_local(async {
            let win = web_sys::window().expect("window");
            let doc = win.document().expect("document");
            let canvas = doc.get_element_by_id("fb_canvas").and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()).expect("canvas");
            // la página se sirve en /s/<token>/app/…; la API cuelga de /s/<token>/api/
            let path = win.location().pathname().unwrap_or_default();
            let mut it = path.split('/').filter(|s| !s.is_empty());
            let api = match (it.next(), it.next()) {
                (Some("s"), Some(tok)) => format!("/s/{tok}/api/"),
                _ => "/api/".to_string(),
            };
            let _ = eframe::WebRunner::new()
                .start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(App::new(cc, api.clone())))))
                .await;
        });
    }
}
