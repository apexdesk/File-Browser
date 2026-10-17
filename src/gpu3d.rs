//! Dibujo del visor 3D con la GPU (OpenGL / OpenGL ES, el mismo contexto que usa egui).
//! La malla se sube una sola vez como búfer de vértices; el sombreado plano de dos luces se calcula en
//! el shader con las derivadas de la posición. Si el contexto no puede (shader no compatible, error de
//! compilación) el visor vuelve al rasterizador por software (`mesh3d::render`).
use crate::mesh3d::Mesh;
use eframe::egui_glow::{self, glow, glow::HasContext, ShaderVersion};
use std::sync::{Arc, Mutex};

pub enum State {
    /// aún no se ha creado nada (se hace en la primera pasada de dibujo)
    Empty,
    Ready(Gpu),
    /// la GPU no sirve: se usa el software
    Failed,
}

pub type Shared = Arc<Mutex<State>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(State::Empty))
}

pub fn failed(s: &Shared) -> bool {
    matches!(*s.lock().unwrap(), State::Failed)
}

/// ¿Aún no se ha intentado crear los recursos de GPU?
pub fn pending(s: &Shared) -> bool {
    matches!(*s.lock().unwrap(), State::Empty)
}

pub struct Gpu {
    gl: Arc<glow::Context>,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    prog: glow::Program,
    n: i32,
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_program(self.prog);
            self.gl.delete_buffer(self.vbo);
            self.gl.delete_vertex_array(self.vao);
        }
    }
}

const VS: &str = "
in vec3 a_pos;
uniform vec3 u_center;
uniform vec4 u_rot;
uniform vec2 u_scale;
uniform float u_inv;
out vec3 v_pos;
void main() {
    vec3 p = a_pos - u_center;
    float x1 = p.x * u_rot.y - p.y * u_rot.x;
    float y1 = p.x * u_rot.x + p.y * u_rot.y;
    vec3 r = vec3(x1, p.z * u_rot.w + y1 * u_rot.z, y1 * u_rot.w - p.z * u_rot.z);
    v_pos = r;
    gl_Position = vec4(r.x * u_scale.x, r.y * u_scale.y, r.z * u_inv, 1.0);
}
";

const FS: &str = "
in vec3 v_pos;
uniform vec3 u_base;
out vec4 o_color;
void main() {
    vec3 n = cross(dFdx(v_pos), dFdy(v_pos));
    float l = length(n);
    if (l < 1e-30) { discard; }
    n /= l;
    vec3 l1 = normalize(vec3(-0.35, 0.55, -0.75));
    vec3 l2 = normalize(vec3(0.6, -0.2, -0.4));
    float lit = min(abs(dot(n, l1)) * 0.75 + abs(dot(n, l2)) * 0.25, 1.0);
    o_color = vec4(u_base * (0.22 + 0.78 * lit), 1.0);
}
";

fn build(gl: &Arc<glow::Context>, m: &Mesh) -> Option<Gpu> {
    let (vs_head, fs_head) = match ShaderVersion::get(gl) {
        ShaderVersion::Gl140 => ("#version 140\n", "#version 140\n"),
        ShaderVersion::Es300 => (
            "#version 300 es\nprecision highp float;\n",
            "#version 300 es\n#ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n#else\nprecision mediump float;\n#endif\n",
        ),
        // GL 2.x / ES 2: sin derivadas ni VAO, lo dejamos al software
        _ => return None,
    };
    unsafe {
        let prog = gl.create_program().ok()?;
        let mut shaders = Vec::new();
        for (kind, head, src) in [(glow::VERTEX_SHADER, vs_head, VS), (glow::FRAGMENT_SHADER, fs_head, FS)] {
            let sh = gl.create_shader(kind).ok()?;
            gl.shader_source(sh, &format!("{head}{src}"));
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                gl.delete_shader(sh);
                for s in shaders {
                    gl.delete_shader(s);
                }
                gl.delete_program(prog);
                return None;
            }
            gl.attach_shader(prog, sh);
            shaders.push(sh);
        }
        gl.bind_attrib_location(prog, 0, "a_pos");
        gl.link_program(prog);
        let linked = gl.get_program_link_status(prog);
        for s in shaders {
            gl.detach_shader(prog, s);
            gl.delete_shader(s);
        }
        if !linked {
            gl.delete_program(prog);
            return None;
        }
        let vao = gl.create_vertex_array().ok()?;
        let vbo = gl.create_buffer().ok()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let bytes = std::slice::from_raw_parts(m.tris.as_ptr() as *const u8, m.tris.len() * 4);
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 12, 0);
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        if gl.get_error() != glow::NO_ERROR {
            gl.delete_program(prog);
            gl.delete_buffer(vbo);
            gl.delete_vertex_array(vao);
            return None;
        }
        Some(Gpu { gl: gl.clone(), vao, vbo, prog, n: (m.tris.len() / 3) as i32 })
    }
}

#[derive(Clone, Copy)]
pub struct Cam {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    /// color base de la malla (0..1)
    pub base: [f32; 3],
}

/// Callback de dibujo para `egui::Shape::Callback`.
pub fn callback(rect: egui::Rect, mesh: Arc<Mesh>, state: Shared, cam: Cam) -> egui::PaintCallback {
    let cb = egui_glow::CallbackFn::new(move |info, painter| {
        let gl = painter.gl();
        let mut st = state.lock().unwrap();
        if matches!(*st, State::Empty) {
            *st = match build(gl, &mesh) {
                Some(g) => State::Ready(g),
                None => State::Failed,
            };
            if std::env::var_os("FB_SHOT").is_some() {
                eprintln!("[trace] visor 3D: {}", if matches!(*st, State::Ready(_)) { "GPU" } else { "software (GPU no disponible)" });
            }
        }
        let State::Ready(g) = &*st else { return };
        let vp = info.viewport_in_pixels();
        let (w, h) = (vp.width_px.max(1) as f32, vp.height_px.max(1) as f32);
        let (sy, cy) = cam.yaw.sin_cos();
        let (sp, cp) = cam.pitch.sin_cos();
        let scale = 0.9 * w.min(h) / (2.0 * mesh.radius) * cam.zoom;
        unsafe {
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
            gl.disable(glow::CULL_FACE);
            gl.use_program(Some(g.prog));
            let loc = |n: &str| gl.get_uniform_location(g.prog, n);
            gl.uniform_3_f32(loc("u_center").as_ref(), mesh.center[0], mesh.center[1], mesh.center[2]);
            gl.uniform_4_f32(loc("u_rot").as_ref(), sy, cy, sp, cp);
            gl.uniform_2_f32(loc("u_scale").as_ref(), 2.0 * scale / w, 2.0 * scale / h);
            gl.uniform_1_f32(loc("u_inv").as_ref(), 1.0 / (1.5 * mesh.radius));
            gl.uniform_3_f32(loc("u_base").as_ref(), cam.base[0], cam.base[1], cam.base[2]);
            gl.bind_vertex_array(Some(g.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, g.n);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::DEPTH_TEST);
        }
    });
    egui::PaintCallback { rect, callback: Arc::new(cb) }
}
