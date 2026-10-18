//! Sin visor 3D por GPU en la web (el contexto WebGL no se puede compartir con el callback): se usa
//! el dibujo por software que ya hay como alternativa.
use crate::mesh3d::Mesh;
use std::sync::{Arc, Mutex};

pub enum State {
    Failed,
}

pub type Shared = Arc<Mutex<State>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(State::Failed))
}

pub fn failed(_s: &Shared) -> bool {
    true
}

pub fn pending(_s: &Shared) -> bool {
    false
}

#[derive(Clone, Copy)]
pub struct Cam {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub base: [f32; 3],
}

pub fn callback(_rect: egui::Rect, _mesh: Arc<Mesh>, _state: Shared, _cam: Cam) -> egui::PaintCallback {
    unreachable!("failed() es siempre cierto")
}
