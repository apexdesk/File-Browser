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
    /// modelo glTF con materiales PBR y texturas
    Pbr(PbrGpu),
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
            *st = match mesh.pbr.as_deref().and_then(|p| build_pbr(gl, p)) {
                Some(g) => State::Pbr(g),
                None => match build(gl, &mesh) {
                    Some(g) => State::Ready(g),
                    None => State::Failed,
                },
            };
            if std::env::var_os("FB_SHOT").is_some() {
                eprintln!("[trace] visor 3D: {}", match &*st { State::Pbr(_) => "GPU (PBR)", State::Ready(_) => "GPU", _ => "software (GPU no disponible)" });
            }
        }
        let vp = info.viewport_in_pixels();
        let (w, h) = (vp.width_px.max(1) as f32, vp.height_px.max(1) as f32);
        let (sy, cy) = cam.yaw.sin_cos();
        let (sp, cp) = cam.pitch.sin_cos();
        let scale = 0.9 * w.min(h) / (2.0 * mesh.radius) * cam.zoom;
        if let State::Pbr(g) = &*st {
            g.draw(&mesh, (sy, cy, sp, cp), scale, (w, h));
            return;
        }
        let State::Ready(g) = &*st else { return };
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

// ---------------------------------------------------------------------------
// PBR (glTF): metal/rugosidad de Cook-Torrance con entorno procedural y texturas
// ---------------------------------------------------------------------------

const PBR_VS: &str = "
in vec3 a_pos;
in vec3 a_nrm;
in vec2 a_uv;
in vec4 a_col;
uniform vec3 u_center;
uniform vec4 u_rot;
uniform vec2 u_scale;
uniform float u_inv;
out vec3 v_pos;
out vec3 v_nrm;
out vec2 v_uv;
out vec4 v_col;
vec3 rot(vec3 p) {
    float x1 = p.x * u_rot.y - p.y * u_rot.x;
    float y1 = p.x * u_rot.x + p.y * u_rot.y;
    return vec3(x1, p.z * u_rot.w + y1 * u_rot.z, y1 * u_rot.w - p.z * u_rot.z);
}
void main() {
    vec3 r = rot(a_pos - u_center);
    v_pos = r;
    v_nrm = rot(a_nrm);
    v_uv = a_uv;
    v_col = a_col;
    gl_Position = vec4(r.x * u_scale.x, r.y * u_scale.y, r.z * u_inv, 1.0);
}
";

const PBR_FS: &str = "
in vec3 v_pos;
in vec3 v_nrm;
in vec2 v_uv;
in vec4 v_col;
uniform vec4 u_rot;
uniform vec4 u_base;
uniform vec2 u_mr;
uniform vec3 u_emis;
uniform int u_flags;
uniform int u_mode;
uniform float u_cut;
uniform float u_nscale;
uniform float u_ostr;
uniform sampler2D u_t0;
uniform sampler2D u_t1;
uniform sampler2D u_t2;
uniform sampler2D u_t3;
uniform sampler2D u_t4;
out vec4 o_color;
const float PI = 3.14159265;
vec3 rot(vec3 p) {
    float x1 = p.x * u_rot.y - p.y * u_rot.x;
    float y1 = p.x * u_rot.x + p.y * u_rot.y;
    return vec3(x1, p.z * u_rot.w + y1 * u_rot.z, y1 * u_rot.w - p.z * u_rot.z);
}
vec3 srgb(vec3 c) { return pow(c, vec3(2.2)); }
// entorno procedural: cielo, horizonte, suelo y una caja de luz
vec3 env(vec3 d, float rough, vec3 up) {
    float h = dot(d, up);
    vec3 sky = mix(vec3(0.75, 0.82, 0.92), vec3(0.25, 0.42, 0.75), clamp(h, 0.0, 1.0));
    vec3 gnd = mix(vec3(0.42, 0.40, 0.38), vec3(0.14, 0.13, 0.12), clamp(-h, 0.0, 1.0));
    vec3 c = h > 0.0 ? sky : gnd;
    float soft = mix(smoothstep(-0.35, 0.35, h), 0.5, rough);
    c = mix(gnd, sky, soft);
    vec3 sun = normalize(vec3(-0.35, 0.55, -0.75));
    float s = max(dot(d, sun), 0.0);
    float k = mix(900.0, 6.0, rough * rough);
    c += vec3(1.0, 0.95, 0.85) * pow(s, k) * mix(6.0, 0.6, rough);
    return c;
}
float dist_ggx(float nh, float a) { float a2 = a * a; float d = nh * nh * (a2 - 1.0) + 1.0; return a2 / (PI * d * d + 1e-7); }
float vis_smith(float nl, float nv, float a) {
    float k = a * 0.5;
    return 0.25 / ((nl * (1.0 - k) + k) * (nv * (1.0 - k) + k) + 1e-7);
}
void main() {
    vec4 base = u_base * v_col;
    if ((u_flags & 1) != 0) { vec4 t = texture(u_t0, v_uv); base *= vec4(srgb(t.rgb), t.a); }
    if (u_mode == 1 && base.a < u_cut) { discard; }
    float alpha = u_mode == 2 ? base.a : 1.0;
    float metal = u_mr.x;
    float rough = u_mr.y;
    if ((u_flags & 2) != 0) { vec3 t = texture(u_t1, v_uv).rgb; rough *= t.g; metal *= t.b; }
    rough = clamp(rough, 0.045, 1.0);
    metal = clamp(metal, 0.0, 1.0);
    vec3 n = normalize(v_nrm);
    vec3 v = vec3(0.0, 0.0, -1.0);
    if ((u_flags & 4) != 0) {
        vec3 dp1 = dFdx(v_pos); vec3 dp2 = dFdy(v_pos);
        vec2 du1 = dFdx(v_uv); vec2 du2 = dFdy(v_uv);
        vec3 dp2p = cross(dp2, n); vec3 dp1p = cross(n, dp1);
        vec3 t = dp2p * du1.x + dp1p * du2.x;
        vec3 b = dp2p * du1.y + dp1p * du2.y;
        float inv = inversesqrt(max(max(dot(t, t), dot(b, b)), 1e-30));
        vec3 m = texture(u_t2, v_uv).xyz * 2.0 - 1.0;
        m.xy *= u_nscale;
        n = normalize(t * inv * m.x + b * inv * m.y + n * m.z);
    }
    if (dot(n, v) < 0.0) { n = -n; }
    vec3 up = rot(vec3(0.0, 0.0, 1.0));
    float ao = 1.0;
    if ((u_flags & 8) != 0) { ao = 1.0 + u_ostr * (texture(u_t3, v_uv).r - 1.0); }
    vec3 albedo = base.rgb;
    vec3 f0 = mix(vec3(0.04), albedo, metal);
    vec3 diff = albedo * (1.0 - metal);
    float nv = max(dot(n, v), 1e-3);
    float a = rough * rough;
    vec3 col = vec3(0.0);
    vec3 lights[2] = vec3[2](normalize(vec3(-0.35, 0.55, -0.75)), normalize(vec3(0.6, -0.2, -0.4)));
    float power[2] = float[2](3.2, 1.0);
    for (int i = 0; i < 2; i++) {
        vec3 l = lights[i];
        float nl = max(dot(n, l), 0.0);
        vec3 hh = normalize(l + v);
        float nh = max(dot(n, hh), 0.0);
        float vh = max(dot(v, hh), 0.0);
        vec3 f = f0 + (1.0 - f0) * pow(1.0 - vh, 5.0);
        vec3 spec = f * dist_ggx(nh, a) * vis_smith(nl, nv, a);
        col += (diff / PI * (1.0 - f) + spec) * nl * power[i] * PI * 0.35;
    }
    // luz ambiente del entorno
    vec3 irr = env(n, 1.0, up) * 0.55;
    vec3 r = reflect(-v, n);
    vec3 fe = f0 + (max(vec3(1.0 - rough), f0) - f0) * pow(1.0 - nv, 5.0);
    col += diff * irr * ao + env(r, rough, up) * fe * ao * 0.8;
    vec3 em = u_emis;
    if ((u_flags & 16) != 0) { em *= srgb(texture(u_t4, v_uv).rgb); }
    col += em;
    col = col / (col + vec3(1.0)) * 1.25;
    col = pow(clamp(col, 0.0, 1.0), vec3(1.0 / 2.2));
    o_color = vec4(col, alpha);
}
";

struct DrawRange {
    first: i32,
    count: i32,
    material: usize,
}

pub struct PbrGpu {
    gl: Arc<glow::Context>,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    prog: glow::Program,
    tex: Vec<Option<glow::Texture>>,
    white: glow::Texture,
    ranges: Vec<DrawRange>,
}

impl Drop for PbrGpu {
    fn drop(&mut self) {
        unsafe {
            for t in self.tex.iter().flatten() {
                self.gl.delete_texture(*t);
            }
            self.gl.delete_texture(self.white);
            self.gl.delete_program(self.prog);
            self.gl.delete_buffer(self.vbo);
            self.gl.delete_buffer(self.ebo);
            self.gl.delete_vertex_array(self.vao);
        }
    }
}

fn shader_heads(gl: &glow::Context) -> Option<(&'static str, &'static str)> {
    match ShaderVersion::get(gl) {
        ShaderVersion::Gl140 => Some(("#version 140\n", "#version 140\n")),
        ShaderVersion::Es300 => Some((
            "#version 300 es\nprecision highp float;\n",
            "#version 300 es\n#ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n#else\nprecision mediump float;\n#endif\n",
        )),
        _ => None,
    }
}

fn build_pbr(gl: &Arc<glow::Context>, p: &crate::pbr::Pbr) -> Option<PbrGpu> {
    use crate::pbr::STRIDE;
    let (vs_head, fs_head) = shader_heads(gl)?;
    let mut verts: Vec<f32> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let mut ranges = Vec::new();
    for pr in &p.prims {
        let base = (verts.len() / STRIDE) as u32;
        verts.extend_from_slice(&pr.verts);
        let first = idx.len() as i32;
        idx.extend(pr.indices.iter().map(|i| i + base));
        ranges.push(DrawRange { first, count: pr.indices.len() as i32, material: pr.material });
    }
    if idx.is_empty() {
        return None;
    }
    unsafe {
        let prog = gl.create_program().ok()?;
        let mut shaders = Vec::new();
        for (kind, head, src) in [(glow::VERTEX_SHADER, vs_head, PBR_VS), (glow::FRAGMENT_SHADER, fs_head, PBR_FS)] {
            let sh = gl.create_shader(kind).ok()?;
            gl.shader_source(sh, &format!("{head}{src}"));
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                if std::env::var_os("FB_SHOT").is_some() {
                    eprintln!("[trace] shader PBR: {}", gl.get_shader_info_log(sh));
                }
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
        for (i, n) in ["a_pos", "a_nrm", "a_uv", "a_col"].iter().enumerate() {
            gl.bind_attrib_location(prog, i as u32, n);
        }
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
        let ebo = gl.create_buffer().ok()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, std::slice::from_raw_parts(verts.as_ptr() as *const u8, verts.len() * 4), glow::STATIC_DRAW);
        let stride = (STRIDE * 4) as i32;
        for (loc, size, off) in [(0u32, 3, 0), (1, 3, 12), (2, 2, 24), (3, 4, 32)] {
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, size, glow::FLOAT, false, stride, off);
        }
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
        gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, std::slice::from_raw_parts(idx.as_ptr() as *const u8, idx.len() * 4), glow::STATIC_DRAW);
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);

        let upload = |w: i32, h: i32, px: &[u8], ws: u32, wt: u32, mips: bool| -> Option<glow::Texture> {
            let t = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(px)));
            if mips {
                gl.generate_mipmap(glow::TEXTURE_2D);
            }
            let min = if mips { glow::LINEAR_MIPMAP_LINEAR } else { glow::LINEAR };
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, min as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, ws as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, wt as i32);
            Some(t)
        };
        let white = upload(1, 1, &[255, 255, 255, 255], glow::CLAMP_TO_EDGE, glow::CLAMP_TO_EDGE, false)?;
        // sólo las texturas que algún material usa
        let mut wanted = vec![false; p.textures.len()];
        for m in &p.materials {
            for t in [m.base_tex, m.mr_tex, m.normal_tex, m.occlusion_tex, m.emissive_tex].into_iter().flatten() {
                wanted[t] = true;
            }
        }
        let mut tex = Vec::new();
        for (i, t) in p.textures.iter().enumerate() {
            let img = p.images.get(t.image).filter(|_| wanted[i]);
            tex.push(img.and_then(|im| upload(im.w as i32, im.h as i32, &im.rgba, t.wrap_s, t.wrap_t, true)));
        }
        gl.bind_texture(glow::TEXTURE_2D, None);
        if gl.get_error() != glow::NO_ERROR {
            for t in tex.iter().flatten() {
                gl.delete_texture(*t);
            }
            gl.delete_texture(white);
            gl.delete_program(prog);
            gl.delete_buffer(vbo);
            gl.delete_buffer(ebo);
            gl.delete_vertex_array(vao);
            return None;
        }
        Some(PbrGpu { gl: gl.clone(), vao, vbo, ebo, prog, tex, white, ranges })
    }
}

impl PbrGpu {
    fn draw(&self, mesh: &Mesh, rot: (f32, f32, f32, f32), scale: f32, size: (f32, f32)) {
        let Some(p) = mesh.pbr.as_deref() else { return };
        let gl = &*self.gl;
        let (w, h) = size;
        unsafe {
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.disable(glow::CULL_FACE);
            gl.use_program(Some(self.prog));
            let loc = |n: &str| gl.get_uniform_location(self.prog, n);
            gl.uniform_3_f32(loc("u_center").as_ref(), mesh.center[0], mesh.center[1], mesh.center[2]);
            gl.uniform_4_f32(loc("u_rot").as_ref(), rot.0, rot.1, rot.2, rot.3);
            gl.uniform_2_f32(loc("u_scale").as_ref(), 2.0 * scale / w, 2.0 * scale / h);
            gl.uniform_1_f32(loc("u_inv").as_ref(), 1.0 / (1.5 * mesh.radius));
            for i in 0..5 {
                gl.uniform_1_i32(loc(&format!("u_t{i}")).as_ref(), i);
            }
            let (l_base, l_mr, l_emis, l_flags, l_mode, l_cut, l_ns, l_os) =
                (loc("u_base"), loc("u_mr"), loc("u_emis"), loc("u_flags"), loc("u_mode"), loc("u_cut"), loc("u_nscale"), loc("u_ostr"));
            gl.bind_vertex_array(Some(self.vao));
            // primero opacos y recortes; luego los translúcidos sin escribir profundidad
            for pass in 0..2 {
                gl.depth_mask(pass == 0);
                if pass == 1 {
                    gl.enable(glow::BLEND);
                    gl.blend_func_separate(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                } else {
                    gl.disable(glow::BLEND);
                }
                for r in &self.ranges {
                    let Some(m) = p.materials.get(r.material) else { continue };
                    if (m.alpha_mode == 2) != (pass == 1) {
                        continue;
                    }
                    let slots = [m.base_tex, m.mr_tex, m.normal_tex, m.occlusion_tex, m.emissive_tex];
                    let mut flags = 0;
                    for (u, s) in slots.iter().enumerate() {
                        let t = s.and_then(|i| self.tex.get(i).copied().flatten());
                        gl.active_texture(glow::TEXTURE0 + u as u32);
                        gl.bind_texture(glow::TEXTURE_2D, Some(t.unwrap_or(self.white)));
                        if t.is_some() {
                            flags |= 1 << u;
                        }
                    }
                    gl.uniform_4_f32(l_base.as_ref(), m.base[0], m.base[1], m.base[2], m.base[3]);
                    gl.uniform_2_f32(l_mr.as_ref(), m.metallic, m.roughness);
                    gl.uniform_3_f32(l_emis.as_ref(), m.emissive[0], m.emissive[1], m.emissive[2]);
                    gl.uniform_1_i32(l_flags.as_ref(), flags);
                    gl.uniform_1_i32(l_mode.as_ref(), m.alpha_mode as i32);
                    gl.uniform_1_f32(l_cut.as_ref(), m.cutoff);
                    gl.uniform_1_f32(l_ns.as_ref(), m.normal_scale);
                    gl.uniform_1_f32(l_os.as_ref(), m.occlusion_strength);
                    gl.draw_elements(glow::TRIANGLES, r.count, glow::UNSIGNED_INT, r.first * 4);
                }
            }
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
        }
    }
}
