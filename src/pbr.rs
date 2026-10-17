//! Datos de un modelo glTF con materiales PBR (metal/rugosidad) y texturas, listos para subir a la GPU.
//! Los vértices ya están en el espacio del visor (Z arriba) con las transformaciones de los nodos aplicadas.

pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

/// Textura = imagen + modo de repetición (códigos de OpenGL).
pub struct Tex {
    pub image: usize,
    pub wrap_s: u32,
    pub wrap_t: u32,
}

pub struct Material {
    pub base: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub base_tex: Option<usize>,
    pub mr_tex: Option<usize>,
    pub normal_tex: Option<usize>,
    pub occlusion_tex: Option<usize>,
    pub emissive_tex: Option<usize>,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    /// 0 opaco, 1 recorte por alfa, 2 mezcla
    pub alpha_mode: u8,
    pub cutoff: f32,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            base: [1.0; 4],
            metallic: 0.0,
            roughness: 0.7,
            emissive: [0.0; 3],
            base_tex: None,
            mr_tex: None,
            normal_tex: None,
            occlusion_tex: None,
            emissive_tex: None,
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            alpha_mode: 0,
            cutoff: 0.5,
        }
    }
}

/// Floats por vértice: posición (3), normal (3), uv (2), color (4).
pub const STRIDE: usize = 12;

pub struct Prim {
    pub verts: Vec<f32>,
    pub indices: Vec<u32>,
    pub material: usize,
}

#[derive(Default)]
pub struct Pbr {
    pub images: Vec<Image>,
    pub textures: Vec<Tex>,
    pub materials: Vec<Material>,
    pub prims: Vec<Prim>,
}
