//! Sin terminal integrada en el navegador: no hay procesos que lanzar.
use crate::theme::Pal;
use egui::Rect;
use std::path::Path;

pub struct Terminal;

pub fn default_shell() -> String {
    String::new()
}

impl Terminal {
    pub fn spawn(_ctx: &egui::Context, _cwd: &Path, _shell: &str) -> Result<Terminal, String> {
        Err("no disponible en la web".into())
    }

    pub fn alive(&self) -> bool {
        false
    }

    pub fn send(&mut self, _bytes: &[u8]) {}

    pub fn show(&mut self, _ui: &mut egui::Ui, _rect: Rect, _pal: &Pal, _focused: bool) {}
}
