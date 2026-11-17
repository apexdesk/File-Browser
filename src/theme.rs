//! Paleta, estilo global y todos los iconos (dibujados con vectores: sin fuentes ni imágenes).
use crate::fsys::Kind;
use egui::{pos2, vec2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Paleta. Los nueve primeros colores son los del tema (editables); el resto se deriva de ellos en
/// `derive` para que texto, bordes de controles, foco y avisos se lean (WCAG 2.x) en cualquier tema.
#[derive(Clone, Copy)]
pub struct Pal {
    pub bg: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub hover: Color32,
    /// borde decorativo (separadores, contorno de tarjetas)
    pub border: Color32,
    pub text: Color32,
    pub subtle: Color32,
    pub accent: Color32,
    pub sel: Color32,
    pub page: Color32,
    /// contorno de controles (campos de texto, casillas, botones): ≥ 3:1 sobre fondo, panel y superficie
    pub outline: Color32,
    /// anillo de foco e indicador de selección: ≥ 3:1 sobre fondo, panel, superficie y resaltado
    pub focus: Color32,
    /// iconos inactivos que aún informan (p. ej. «Atrás» sin historial): ≥ 3:1
    pub disabled: Color32,
    /// texto sobre un relleno de acento (botón principal)
    pub on_accent: Color32,
    /// texto de error: ≥ 4.5:1 sobre fondo, panel, superficie y resaltado
    pub danger: Color32,
    /// texto de aviso: ≥ 4.5:1 sobre fondo, panel, superficie y resaltado
    pub warn: Color32,
    /// relleno de botón destructivo y su texto
    pub danger_fill: Color32,
    pub on_danger: Color32,
}

impl Pal {
    /// Mezcla lineal de dos paletas (t = 0 → `self`, t = 1 → `o`), para animar el cambio de tema.
    /// Si el sistema pide reducir el movimiento no hay transición: se devuelve directamente `o`.
    pub fn lerp(&self, o: &Pal, t: f32) -> Pal {
        if reduced_motion() {
            return *o;
        }
        let m = |a: Color32, b: Color32| {
            let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
            Color32::from_rgba_premultiplied(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()), f(a.a(), b.a()))
        };
        Pal {
            bg: m(self.bg, o.bg),
            panel: m(self.panel, o.panel),
            surface: m(self.surface, o.surface),
            hover: m(self.hover, o.hover),
            border: m(self.border, o.border),
            text: m(self.text, o.text),
            subtle: m(self.subtle, o.subtle),
            accent: m(self.accent, o.accent),
            sel: m(self.sel, o.sel),
            page: m(self.page, o.page),
            outline: m(self.outline, o.outline),
            focus: m(self.focus, o.focus),
            disabled: m(self.disabled, o.disabled),
            // el texto sobre relleno salta de golpe (blanco o negro): una mezcla gris no se leería
            on_accent: if t < 0.5 { self.on_accent } else { o.on_accent },
            danger: m(self.danger, o.danger),
            warn: m(self.warn, o.warn),
            danger_fill: m(self.danger_fill, o.danger_fill),
            on_danger: if t < 0.5 { self.on_danger } else { o.on_danger },
        }
    }
}

// ---------------------------------------------------------------------------
// Contraste (WCAG 2.x)
// ---------------------------------------------------------------------------

/// Contraste mínimo del texto normal (WCAG 1.4.3).
pub const MIN_TEXT_CONTRAST: f32 = 4.5;
/// Contraste mínimo de bordes de controles, estados, foco e iconos que informan (WCAG 1.4.11).
pub const MIN_UI_CONTRAST: f32 = 3.0;

fn lin(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}

/// Luminancia relativa (WCAG) de un color; los translúcidos cuentan como compuestos sobre negro
/// (para otro fondo, componer antes con `over`).
pub fn luminance(c: Color32) -> f32 {
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// Relación de contraste WCAG entre dos colores (1 … 21).
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Compone un color translúcido (premultiplicado, como `Color32`) sobre un fondo opaco.
pub fn over(fg: Color32, bg: Color32) -> Color32 {
    let k = 1.0 - fg.a() as f32 / 255.0;
    let f = |x: u8, y: u8| (x as f32 + y as f32 * k).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(fg.r(), bg.r()), f(fg.g(), bg.g()), f(fg.b(), bg.b()))
}

/// Texto legible sobre un relleno opaco: blanco si llega a 4.5:1, si no el que más contraste dé (negro).
/// Siempre da ≥ 4.5:1 (el peor caso, un gris medio, da 4.58 con negro).
pub fn on_color(bg: Color32) -> Color32 {
    let w = contrast_ratio(Color32::WHITE, bg);
    if w >= MIN_TEXT_CONTRAST || w >= contrast_ratio(Color32::BLACK, bg) { Color32::WHITE } else { Color32::BLACK }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Acerca `fg` a blanco o a negro (hacia lo que más ayude) lo justo para que tenga al menos `min` de
/// contraste con todos los fondos de `bgs`. Si ya lo cumple se devuelve igual; si es imposible, el extremo.
pub fn ensure_contrast_all(fg: Color32, bgs: &[Color32], min: f32) -> Color32 {
    let worst = |c: Color32| bgs.iter().map(|b| contrast_ratio(c, *b)).fold(f32::INFINITY, f32::min);
    let fg = Color32::from_rgb(fg.r(), fg.g(), fg.b());
    if bgs.is_empty() || worst(fg) >= min {
        return fg;
    }
    let to = if worst(Color32::WHITE) >= worst(Color32::BLACK) { Color32::WHITE } else { Color32::BLACK };
    for i in 1..=100 {
        let c = mix(fg, to, i as f32 / 100.0);
        if worst(c) >= min {
            return c;
        }
    }
    to
}

/// Como `ensure_contrast_all` con un solo fondo.
pub fn ensure_contrast(fg: Color32, bg: Color32, min: f32) -> Color32 {
    ensure_contrast_all(fg, &[bg], min)
}

/// Completa una paleta: corrige texto, texto secundario y acento para que se lean (≥ 4.5:1) sobre fondo,
/// panel, superficie, resaltado y selección, y deriva contorno de controles, foco, iconos inactivos y
/// colores de error y aviso. Cambia lo mínimo: un color que ya cumple se queda como está.
fn derive(base: Pal, dark: bool) -> Pal {
    // margen pequeño para que el redondeo a 8 bits no deje un par justo por debajo del umbral
    const EPS: f32 = 0.03;
    let mut p = base;
    let surface = p.surface;
    // tinte de selección: el acento translúcido; con acentos muy claros (oscuro) o muy oscuros (claro) se
    // rebaja la opacidad para que el tinte no se acerque al texto y éste siga leyéndose encima
    let sel_of = |ac: Color32| {
        let mut a: u8 = if dark { 64 } else { 50 };
        loop {
            let s = Color32::from_rgba_unmultiplied(ac.r(), ac.g(), ac.b(), a);
            let l = luminance(over(s, surface));
            if a <= 24 || (dark && l <= 0.06) || (!dark && l >= 0.70) {
                return s;
            }
            a -= 2;
        }
    };
    let grounds = [p.bg, p.panel, p.surface];
    // el acento se usa como texto sobre el fondo y sobre la propia selección (que sale de él): se
    // busca el punto fijo para que volver a derivar la paleta no la cambie
    for _ in 0..32 {
        p.sel = sel_of(p.accent);
        let on: Vec<Color32> = grounds.iter().copied().chain(grounds.iter().map(|g| over(p.sel, *g))).collect();
        let a = ensure_contrast_all(p.accent, &on, MIN_TEXT_CONTRAST + EPS);
        if a == p.accent {
            break;
        }
        p.accent = a;
    }
    p.sel = sel_of(p.accent);
    let text_on = [p.bg, p.panel, p.surface, p.hover, over(p.sel, p.bg), over(p.sel, p.panel), over(p.sel, p.surface)];
    p.text = ensure_contrast_all(p.text, &text_on, MIN_TEXT_CONTRAST + EPS);
    p.subtle = ensure_contrast_all(p.subtle, &text_on, MIN_TEXT_CONTRAST + EPS);
    let plain = [p.bg, p.panel, p.surface, p.hover];
    p.outline = ensure_contrast_all(p.border, &grounds, MIN_UI_CONTRAST + EPS);
    p.focus = ensure_contrast_all(p.accent, &plain, MIN_UI_CONTRAST + EPS);
    p.disabled = ensure_contrast_all(mix(p.bg, p.subtle, 0.5), &plain, MIN_UI_CONTRAST + EPS);
    p.on_accent = on_color(p.accent);
    let (red, amber) = if dark {
        (Color32::from_rgb(240, 112, 112), Color32::from_rgb(236, 178, 72))
    } else {
        (Color32::from_rgb(196, 43, 43), Color32::from_rgb(150, 98, 0))
    };
    p.danger = ensure_contrast_all(red, &plain, MIN_TEXT_CONTRAST + EPS);
    p.warn = ensure_contrast_all(amber, &plain, MIN_TEXT_CONTRAST + EPS);
    // rojo de botón con texto blanco (≥ 4.5:1), igual en claro y en oscuro
    p.danger_fill = ensure_contrast(Color32::from_rgb(214, 70, 70), Color32::WHITE, MIN_TEXT_CONTRAST + EPS);
    p.on_danger = on_color(p.danger_fill);
    p
}

/// Paleta a partir de los nueve colores de un tema (fondo, panel, superficie, resaltado, borde, texto,
/// texto secundario, acento, fondo del visor).
fn from_colors(c: [[u8; 3]; 9], dark: bool) -> Pal {
    let k = |i: usize| Color32::from_rgb(c[i][0], c[i][1], c[i][2]);
    let z = Color32::TRANSPARENT;
    derive(
        Pal {
            bg: k(0),
            panel: k(1),
            surface: k(2),
            hover: k(3),
            border: k(4),
            text: k(5),
            subtle: k(6),
            accent: k(7),
            sel: z,
            page: k(8),
            outline: z,
            focus: z,
            disabled: z,
            on_accent: z,
            danger: z,
            warn: z,
            danger_fill: z,
            on_danger: z,
        },
        dark,
    )
}

pub fn palette(dark: bool) -> Pal {
    if dark {
        from_colors([[24, 26, 32], [30, 33, 41], [39, 43, 54], [50, 55, 69], [52, 57, 71], [231, 233, 240], [137, 145, 165], [94, 140, 255], [236, 239, 246]], true)
    } else {
        from_colors([[246, 247, 250], [236, 239, 245], [255, 255, 255], [224, 229, 240], [210, 216, 228], [30, 34, 44], [105, 114, 135], [52, 106, 240], [255, 255, 255]], false)
    }
}

impl Pal {
    pub fn colors(&self) -> [[u8; 3]; 9] {
        let a = |c: Color32| [c.r(), c.g(), c.b()];
        [a(self.bg), a(self.panel), a(self.surface), a(self.hover), a(self.border), a(self.text), a(self.subtle), a(self.accent), a(self.page)]
    }
}

/// Paleta de un tema propio; la selección y los colores de apoyo se derivan de los nueve (ver `derive`).
pub fn palette_of(th: &crate::config::CustomTheme) -> Pal {
    from_colors(th.colors, th.dark)
}

/// Color de los textos de error, legible sobre el fondo del tema (`pal.danger`).
pub fn error_color(p: &Pal) -> Color32 {
    p.danger
}

/// Color de los textos de aviso, legible sobre el fondo del tema (`pal.warn`).
pub fn warn_color(p: &Pal) -> Color32 {
    p.warn
}

// ---------------------------------------------------------------------------
// Movimiento reducido y tamaño mínimo de los controles
// ---------------------------------------------------------------------------

/// 0 = sin consultar, 1 = no, 2 = sí.
static REDUCED_MOTION: AtomicU8 = AtomicU8::new(0);

/// Fuerza (o desactiva) el modo de movimiento reducido: sin transición al cambiar de tema.
pub fn set_reduced_motion(on: bool) {
    REDUCED_MOTION.store(if on { 2 } else { 1 }, Ordering::Relaxed);
}

/// ¿Hay que evitar animaciones? La primera vez se pregunta al sistema (`detect_reduced_motion`), salvo
/// que antes se haya fijado con `set_reduced_motion`.
pub fn reduced_motion() -> bool {
    match REDUCED_MOTION.load(Ordering::Relaxed) {
        0 => {
            let on = detect_reduced_motion();
            // si mientras tanto se fijó a mano, manda lo fijado
            let _ = REDUCED_MOTION.compare_exchange(0, if on { 2 } else { 1 }, Ordering::Relaxed, Ordering::Relaxed);
            REDUCED_MOTION.load(Ordering::Relaxed) == 2
        }
        v => v == 2,
    }
}

/// Preferencia del sistema de reducir las animaciones (egui/eframe 0.36 no la exponen).
/// Windows: «Mostrar animaciones en Windows»; macOS: Accesibilidad › Reducir movimiento;
/// GNOME: `enable-animations`; KDE: velocidad de animación 0. En Android y la web, no.
pub fn detect_reduced_motion() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn SystemParametersInfoW(action: u32, param: u32, pv: *mut core::ffi::c_void, win_ini: u32) -> i32;
        }
        const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
        let mut on: i32 = 1;
        let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut on as *mut i32 as *mut core::ffi::c_void, 0) };
        ok != 0 && on == 0
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("defaults").args(["read", "com.apple.universalaccess", "reduceMotion"]).output();
        matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "1")
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        if run("gsettings", &["get", "org.gnome.desktop.interface", "enable-animations"]).as_deref() == Some("false") {
            return true;
        }
        for k in ["kreadconfig6", "kreadconfig5"] {
            if let Some(v) = run(k, &["--group", "KDE", "--key", "AnimationDurationFactor"]) {
                return v.parse::<f32>().map(|f| f <= 0.0).unwrap_or(false);
            }
        }
        false
    }
    #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "android")))))]
    {
        false
    }
}

/// Lado mínimo de un control que se pulsa con el ratón (WCAG 2.5.8).
pub const MIN_TARGET: f32 = 24.0;
/// Lado mínimo de un control en pantallas táctiles (pautas de Android y Apple: 44-48).
pub const MIN_TARGET_TOUCH: f32 = 44.0;

static TOUCH_UI: AtomicBool = AtomicBool::new(cfg!(target_os = "android"));

/// Marca la interfaz como táctil (controles de al menos `MIN_TARGET_TOUCH`). En Android ya lo está.
pub fn set_touch_ui(on: bool) {
    TOUCH_UI.store(on, Ordering::Relaxed);
}

pub fn touch_ui() -> bool {
    TOUCH_UI.load(Ordering::Relaxed)
}

/// Lado mínimo de un control pulsable en esta plataforma.
pub fn min_target() -> f32 {
    if touch_ui() { MIN_TARGET_TOUCH } else { MIN_TARGET }
}

/// Agranda un tamaño de control hasta el mínimo pulsable (el dibujo puede seguir siendo más pequeño).
pub fn target_size(size: egui::Vec2) -> egui::Vec2 {
    size.max(egui::Vec2::splat(min_target()))
}

/// ¿Se debe pintar el anillo de foco? Sólo cuando el foco llegó con el teclado (Tab o flechas) después
/// del último clic, como `:focus-visible` en la web: un clic con el ratón no deja un anillo pegado.
pub fn focus_visible(ctx: &egui::Context) -> bool {
    let id = egui::Id::new("theme_focus_visible");
    let frame = ctx.cumulative_frame_nr();
    let (last, mut on) = ctx.data(|d| d.get_temp::<(u64, bool)>(id)).unwrap_or((u64::MAX, false));
    if last != frame {
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Key { key, pressed: true, .. }
                        if matches!(key, egui::Key::Tab | egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown) =>
                    {
                        on = true
                    }
                    egui::Event::PointerButton { pressed: true, .. } | egui::Event::Touch { .. } => on = false,
                    _ => {}
                }
            }
        });
        ctx.data_mut(|d| d.insert_temp(id, (frame, on)));
    }
    on
}

/// Anillo de foco (2 pt, ≥ 3:1) alrededor de un control propio que tiene el foco del teclado.
pub fn focus_ring(ui: &egui::Ui, p: &Pal, resp: &egui::Response, radius: f32) {
    // se consulta siempre (no sólo con foco) para no perderse el Tab o el clic de este fotograma
    let visible = focus_visible(ui.ctx());
    if visible && resp.has_focus() {
        let r = CornerRadius::same((radius + 2.0).round().clamp(0.0, 255.0) as u8);
        ui.painter().rect_stroke(resp.rect.expand(2.0), r, Stroke::new(2.0, p.focus), egui::StrokeKind::Outside);
    }
}

/// Paleta vigente: la del tema activo (propio o predefinido) si coincide con el modo, o la de serie.
pub fn resolve(cfg: &crate::config::Config, dark: bool) -> Pal {
    match cfg.theme_named(&cfg.theme_custom) {
        Some(th) => palette_of(&th),
        None => palette(dark),
    }
}

/// Temas predefinidos (de solo lectura; se pueden duplicar para editarlos). Los nombres son nombres propios y
/// no se traducen, porque se guardan en los ajustes.
pub fn presets() -> &'static [crate::config::CustomTheme] {
    static P: std::sync::OnceLock<Vec<crate::config::CustomTheme>> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        // fondo, panel, superficie, resaltado, borde, texto, texto secundario, acento, fondo del visor
        let t = |name: &str, dark: bool, c: [[u8; 3]; 9]| crate::config::CustomTheme { name: name.to_string(), dark, colors: c };
        vec![
            t("Nord", true, [[46, 52, 64], [52, 60, 74], [59, 66, 82], [67, 76, 94], [76, 86, 106], [236, 239, 244], [154, 165, 185], [136, 192, 208], [236, 239, 244]]),
            t("Dracula", true, [[40, 42, 54], [33, 34, 44], [52, 55, 70], [68, 71, 90], [78, 82, 104], [248, 248, 242], [160, 166, 196], [189, 147, 249], [248, 248, 242]]),
            t("Gruvbox Dark", true, [[40, 40, 40], [50, 48, 47], [60, 56, 54], [80, 73, 69], [102, 92, 84], [235, 219, 178], [168, 153, 132], [250, 189, 47], [251, 241, 199]]),
            t("Monokai", true, [[39, 40, 34], [49, 50, 43], [62, 61, 50], [80, 80, 66], [90, 90, 74], [248, 248, 242], [160, 160, 140], [166, 226, 46], [248, 248, 242]]),
            t("Midnight", true, [[0, 0, 0], [10, 10, 14], [20, 21, 28], [34, 36, 48], [40, 42, 56], [230, 232, 240], [128, 134, 155], [100, 160, 255], [236, 239, 246]]),
            t("Solarized Light", false, [[253, 246, 227], [238, 232, 213], [255, 251, 238], [228, 221, 198], [211, 203, 178], [55, 80, 88], [131, 148, 150], [38, 139, 210], [253, 246, 227]]),
            t("Gruvbox Light", false, [[251, 241, 199], [242, 229, 188], [253, 247, 221], [235, 219, 178], [213, 196, 161], [60, 56, 54], [124, 111, 100], [175, 58, 3], [253, 247, 221]]),
            t("Paper", false, [[250, 248, 244], [241, 238, 231], [255, 255, 252], [232, 228, 218], [220, 214, 202], [40, 38, 34], [118, 112, 100], [184, 92, 40], [255, 254, 250]]),
            t("Rosé Pine Dawn", false, [[250, 244, 237], [242, 233, 225], [255, 250, 243], [233, 223, 214], [223, 218, 217], [87, 82, 121], [121, 117, 147], [180, 99, 122], [255, 250, 243]]),
            t("GitHub Light", false, [[255, 255, 255], [246, 248, 250], [250, 251, 252], [234, 238, 242], [208, 215, 222], [31, 35, 40], [101, 109, 118], [9, 105, 218], [255, 255, 255]]),
        ]
    })
}

pub fn apply_style(ctx: &egui::Context, p: &Pal, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.panel;
    v.override_text_color = Some(p.text);
    v.window_corner_radius = CornerRadius::same(16);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0, p.border);
    // sombra amplia y suave: las ventanas «flotan» sobre el contenido
    let a = if dark { 150 } else { 60 };
    v.window_shadow = egui::epaint::Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha(a) };
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(a) };
    v.selection.bg_fill = p.sel;
    // también es el borde de un campo de texto con foco y el texto de un elemento seleccionado
    v.selection.stroke = Stroke::new(2.0, p.accent);
    v.weak_text_color = Some(p.subtle);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(9);
    }
    v.widgets.inactive.bg_fill = p.surface;
    v.widgets.inactive.weak_bg_fill = p.surface;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.active.bg_fill = p.hover;
    v.widgets.active.weak_bg_fill = p.hover;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    // contorno visible (≥ 3:1) en campos de texto, casillas, botones de opción y botones; el estado
    // «activo» es también el de foco del teclado en egui: anillo de 2 pt del color de foco
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.outline);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.outline);
    v.widgets.active.bg_stroke = Stroke::new(2.0, p.focus);
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.outline);
    // marca de las casillas y punto de los botones de opción
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.fg_stroke.color = p.text;
    }
    ctx.set_visuals(v);
    // una muesca de la rueda mueve más y no se suaviza de más (el resto de listas y visores)
    ctx.options_mut(|o| o.input_options.line_scroll_speed = 64.0);
    ctx.global_style_mut(|s| {
        s.animation_time = 0.0;
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.window_margin = egui::Margin::same(16);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.interaction.tooltip_delay = 0.4;
    });
}

// ---------------------------------------------------------------------------
// Glifos de interfaz
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Back,
    Forward,
    Up,
    Refresh,
    Grid,
    List,
    Star,
    StarFilled,
    Folder,
    Network,
    Bookmark,
    BookmarkFilled,
    Gear,
    Home,
    Sun,
    Moon,
    Eye,
    EyeOff,
    FolderPlus,
    Desktop,
    Document,
    Download,
    Picture,
    Music,
    Video,
    Drive,
    Close,
    Menu,
    Minimize,
    Maximize,
    Restore,
    Plus,
    Check,
    More,
    Trash,
    Clock,
    Columns,
    Search,
}

/// Trazo con terminaciones redondeadas (egui no las ofrece): se añaden discos en los extremos.
fn path(p: &Painter, pts: &[Pos2], closed: bool, st: Stroke) {
    if closed {
        p.add(Shape::closed_line(pts.to_vec(), st));
    } else {
        p.add(Shape::line(pts.to_vec(), st));
        let r = st.width / 2.0;
        if let (Some(a), Some(b)) = (pts.first(), pts.last()) {
            p.circle_filled(*a, r, st.color);
            p.circle_filled(*b, r, st.color);
        }
    }
}

fn arc(c: Pos2, r: f32, a0: f32, a1: f32, n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect()
}

fn star_points(c: Pos2, r: f32) -> Vec<Pos2> {
    (0..10)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let rr = if i % 2 == 0 { r } else { r * 0.47 };
            c + vec2(a.cos(), a.sin()) * rr
        })
        .collect()
}

/// Dibuja un glifo en una rejilla de 24 unidades (zona útil de ±10) centrada en `c`.
/// Trazo uniforme, esquinas y extremos redondeados, grosor proporcional al tamaño.
pub fn draw_glyph(p: &Painter, c: Pos2, s: f32, g: Glyph, col: Color32) {
    let u = s / 24.0;
    let w = (s / 12.0).max(1.5);
    let st = Stroke::new(w, col);
    let q = |x: f32, y: f32| c + vec2(x * u, y * u);
    let rr = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(q(x0, y0), q(x1, y1));
    let rad = |r: f32| CornerRadius::same((r * u).round().clamp(0.0, 255.0) as u8);
    let tau = std::f32::consts::TAU;
    match g {
        Glyph::Back => {
            path(p, &[q(9.0, 0.0), q(-9.0, 0.0)], false, st);
            path(p, &[q(-3.0, -6.0), q(-9.0, 0.0), q(-3.0, 6.0)], false, st);
        }
        Glyph::Forward => {
            path(p, &[q(-9.0, 0.0), q(9.0, 0.0)], false, st);
            path(p, &[q(3.0, -6.0), q(9.0, 0.0), q(3.0, 6.0)], false, st);
        }
        Glyph::Up => {
            path(p, &[q(0.0, 9.0), q(0.0, -9.0)], false, st);
            path(p, &[q(-6.0, -3.0), q(0.0, -9.0), q(6.0, -3.0)], false, st);
        }
        Glyph::Refresh => {
            // arco de 280° en sentido horario con punta de flecha rellena al final
            let a0 = 30f32.to_radians();
            let a1 = a0 + 280f32.to_radians();
            let pts: Vec<Pos2> = arc(c, 8.0 * u, a0, a1, 28);
            path(p, &pts, false, st);
            let e = *pts.last().unwrap();
            let tan = vec2(-a1.sin(), a1.cos());
            let nor = vec2(a1.cos(), a1.sin());
            let tip = e + tan * 5.0 * u;
            p.add(Shape::convex_polygon(
                vec![tip, e + nor * 4.2 * u, e - nor * 4.2 * u],
                col,
                Stroke::new(w * 0.5, col),
            ));
        }
        Glyph::Grid => {
            for (x, y) in [(-9.0, -9.0), (1.0, -9.0), (-9.0, 1.0), (1.0, 1.0)] {
                p.rect_stroke(rr(x, y, x + 8.0, y + 8.0), rad(2.5), st, egui::StrokeKind::Inside);
            }
        }
        Glyph::List => {
            // filas con miniatura + línea (vista de lista), distinto del icono de menú
            for y in [-6.5, 0.0, 6.5] {
                p.rect_filled(rr(-10.0, y - 2.4, -5.2, y + 2.4), rad(1.4), col);
                path(p, &[q(-1.0, y), q(10.0, y)], false, st);
            }
        }
        Glyph::Star | Glyph::StarFilled => {
            let ctr = q(0.0, 0.6);
            let pts = star_points(ctr, 10.0 * u);
            if g == Glyph::StarFilled {
                for i in 0..10 {
                    p.add(Shape::convex_polygon(vec![ctr, pts[i], pts[(i + 1) % 10]], col, Stroke::new(0.6, col)));
                }
            }
            p.add(Shape::closed_line(pts, st));
        }
        Glyph::Network => {
            // globo: círculo, ecuador y meridiano
            p.circle_stroke(c, 8.6 * u, st);
            path(p, &[q(-8.6, 0.0), q(8.6, 0.0)], false, st);
            let mer: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 16.0;
                    q(a.cos() * 4.2, a.sin() * 8.6)
                })
                .collect();
            path(p, &mer, false, st);
            let mer2: Vec<Pos2> = mer.iter().map(|m| c + (c - *m)).collect();
            path(p, &mer2, false, st);
        }
        Glyph::Folder => {
            p.add(Shape::closed_line(
                vec![q(-9.5, -6.5), q(-3.5, -6.5), q(-1.2, -3.8), q(9.5, -3.8), q(9.5, 7.5), q(-9.5, 7.5)],
                st,
            ));
        }
        Glyph::Bookmark | Glyph::BookmarkFilled => {
            if g == Glyph::BookmarkFilled {
                p.add(Shape::convex_polygon(vec![q(-5.5, -9.0), q(0.0, -9.0), q(0.0, 4.5), q(-5.5, 9.5)], col, Stroke::new(0.6, col)));
                p.add(Shape::convex_polygon(vec![q(5.5, -9.0), q(0.0, -9.0), q(0.0, 4.5), q(5.5, 9.5)], col, Stroke::new(0.6, col)));
            }
            p.add(Shape::closed_line(vec![q(-5.5, -9.0), q(5.5, -9.0), q(5.5, 9.5), q(0.0, 4.5), q(-5.5, 9.5)], st));
        }
        Glyph::Gear => {
            // engranaje de 8 dientes con base redondeada y círculo central
            let n = 8;
            let mut pts = Vec::new();
            for i in 0..n {
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 * tau / n as f32;
                let half_tip = 0.17;
                let half_base = 0.30;
                pts.push(c + vec2((a - half_base).cos(), (a - half_base).sin()) * 7.3 * u);
                pts.push(c + vec2((a - half_tip).cos(), (a - half_tip).sin()) * 9.9 * u);
                pts.push(c + vec2((a + half_tip).cos(), (a + half_tip).sin()) * 9.9 * u);
                pts.push(c + vec2((a + half_base).cos(), (a + half_base).sin()) * 7.3 * u);
                // arco redondeado hasta el siguiente diente
                let a0 = a + half_base;
                let a1 = a + tau / n as f32 - half_base;
                for k in 1..4 {
                    let aa = a0 + (a1 - a0) * k as f32 / 4.0;
                    pts.push(c + vec2(aa.cos(), aa.sin()) * 7.3 * u);
                }
            }
            p.add(Shape::closed_line(pts, st));
            p.circle_stroke(c, 3.0 * u, st);
        }
        Glyph::Home => {
            path(p, &[q(-9.5, -1.5), q(0.0, -9.5), q(9.5, -1.5)], false, st);
            path(p, &[q(-6.0, -4.3), q(-6.0, 9.0), q(6.0, 9.0), q(6.0, -4.3)], false, st);
            path(p, &[q(-2.2, 9.0), q(-2.2, 3.0), q(2.2, 3.0), q(2.2, 9.0)], false, st);
        }
        Glyph::Sun => {
            p.circle_stroke(c, 4.6 * u, st);
            for i in 0..8 {
                let a = i as f32 * tau / 8.0;
                let d = vec2(a.cos(), a.sin());
                path(p, &[c + d * 7.6 * u, c + d * 10.0 * u], false, st);
            }
        }
        Glyph::Moon => {
            // creciente: arco exterior + arco interior desplazado
            let (cr, rr_) = (q(-0.5, 0.5), 9.0 * u);
            let off = vec2(4.6, -3.8) * u;
            let (ci, ri) = (cr + off, 7.4 * u);
            let away = (-off).angle();
            let outside = |a: f32| (cr + vec2(a.cos(), a.sin()) * rr_ - ci).length() >= ri;
            let (mut s_a, mut e_a) = (away, away);
            while outside(e_a) && e_a - away < tau {
                e_a += 0.03;
            }
            while outside(s_a) && away - s_a < tau {
                s_a -= 0.03;
            }
            let mut pts = arc(cr, rr_, s_a, e_a, 40);
            let toward = off.angle() + std::f32::consts::PI; // lado del círculo interior que mira al centro
            let mut inner: Vec<Pos2> = Vec::new();
            let mut d = -std::f32::consts::PI;
            while d <= std::f32::consts::PI {
                let pt = ci + vec2((toward + d).cos(), (toward + d).sin()) * ri;
                if (pt - cr).length() <= rr_ + 0.01 {
                    inner.push(pt);
                }
                d += 0.05;
            }
            if let (Some(last), Some(f), Some(l)) = (pts.last().copied(), inner.first().copied(), inner.last().copied()) {
                if (last - f).length() > (last - l).length() {
                    inner.reverse();
                }
            }
            pts.extend(inner);
            p.add(Shape::closed_line(pts, st));
        }
        Glyph::Eye | Glyph::EyeOff => {
            let top: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let x = -10.0 + 20.0 * i as f32 / 16.0;
                    q(x, -7.5 * (1.0 - (x / 10.0) * (x / 10.0)))
                })
                .collect();
            let mut shape = top.clone();
            shape.extend(top.iter().rev().map(|a| pos2(a.x, 2.0 * c.y - a.y)));
            p.add(Shape::closed_line(shape, st));
            p.circle_stroke(c, 3.2 * u, st);
            if g == Glyph::EyeOff {
                path(p, &[q(-8.0, 8.0), q(8.0, -8.0)], false, st);
            }
        }
        Glyph::FolderPlus => {
            p.add(Shape::closed_line(
                vec![q(-10.0, -7.0), q(-3.5, -7.0), q(-1.0, -4.0), q(10.0, -4.0), q(10.0, 8.0), q(-10.0, 8.0)],
                st,
            ));
            path(p, &[q(-3.5, 2.5), q(3.5, 2.5)], false, st);
            path(p, &[q(0.0, -1.0), q(0.0, 6.0)], false, st);
        }
        Glyph::Desktop => {
            p.rect_stroke(rr(-10.0, -8.0, 10.0, 4.0), rad(2.5), st, egui::StrokeKind::Inside);
            path(p, &[q(0.0, 4.0), q(0.0, 8.0)], false, st);
            path(p, &[q(-4.5, 8.5), q(4.5, 8.5)], false, st);
        }
        Glyph::Document => {
            p.add(Shape::closed_line(
                vec![q(-7.0, -10.0), q(2.0, -10.0), q(7.0, -5.0), q(7.0, 10.0), q(-7.0, 10.0)],
                st,
            ));
            path(p, &[q(2.0, -10.0), q(2.0, -5.0), q(7.0, -5.0)], false, st);
            path(p, &[q(-3.0, 1.5), q(3.0, 1.5)], false, st);
            path(p, &[q(-3.0, 5.5), q(3.0, 5.5)], false, st);
        }
        Glyph::Download => {
            path(p, &[q(0.0, -9.0), q(0.0, 3.0)], false, st);
            path(p, &[q(-5.5, -2.5), q(0.0, 3.0), q(5.5, -2.5)], false, st);
            path(p, &[q(-9.0, 4.0), q(-9.0, 9.0), q(9.0, 9.0), q(9.0, 4.0)], false, st);
        }
        Glyph::Picture => {
            p.rect_stroke(rr(-10.0, -8.0, 10.0, 8.0), rad(3.0), st, egui::StrokeKind::Inside);
            path(p, &[q(-9.5, 5.5), q(-3.5, -1.0), q(1.0, 3.5), q(4.0, 0.5), q(9.5, 5.5)], false, st);
            p.circle_stroke(q(5.0, -3.5), 1.8 * u, st);
        }
        Glyph::Music => {
            p.circle_filled(q(-1.5, 6.0), 3.4 * u, col);
            path(p, &[q(1.9, 6.0), q(1.9, -9.0)], false, st);
            path(p, &[q(1.9, -9.0), q(7.0, -6.5), q(8.5, -2.5)], false, st);
        }
        Glyph::Video => {
            p.rect_stroke(rr(-10.0, -7.0, 10.0, 7.0), rad(3.0), st, egui::StrokeKind::Inside);
            p.add(Shape::convex_polygon(
                vec![q(-2.4, -3.8), q(4.4, 0.0), q(-2.4, 3.8)],
                col,
                Stroke::new(w * 0.6, col),
            ));
        }
        Glyph::Drive => {
            p.rect_stroke(rr(-10.0, -5.5, 10.0, 7.0), rad(3.0), st, egui::StrokeKind::Inside);
            path(p, &[q(-10.0, 0.5), q(10.0, 0.5)], false, st);
            p.circle_filled(q(5.8, 3.8), 1.3 * u, col);
        }
        Glyph::Close => {
            path(p, &[q(-6.0, -6.0), q(6.0, 6.0)], false, st);
            path(p, &[q(-6.0, 6.0), q(6.0, -6.0)], false, st);
        }
        Glyph::Menu => {
            for y in [-6.0, 0.0, 6.0] {
                path(p, &[q(-9.0, y), q(9.0, y)], false, st);
            }
        }
        Glyph::Minimize => {
            path(p, &[q(-8.0, 3.0), q(8.0, 3.0)], false, st);
        }
        Glyph::Maximize => {
            p.rect_stroke(rr(-8.0, -8.0, 8.0, 8.0), rad(2.5), st, egui::StrokeKind::Inside);
        }
        Glyph::Restore => {
            p.rect_stroke(rr(-8.0, -3.0, 3.0, 8.0), rad(2.5), st, egui::StrokeKind::Inside);
            path(p, &[q(-3.0, -3.0), q(-3.0, -6.0), q(0.5, -8.0), q(8.0, -8.0), q(8.0, 0.0), q(3.0, 0.0)], false, st);
        }
        Glyph::Plus => {
            path(p, &[q(-8.0, 0.0), q(8.0, 0.0)], false, st);
            path(p, &[q(0.0, -8.0), q(0.0, 8.0)], false, st);
        }
        Glyph::More => {
            for y in [-6.0, 0.0, 6.0] {
                p.circle_filled(q(0.0, y), 1.9, st.color);
            }
        }
        Glyph::Columns => {
            path(p, &[q(-8.0, -7.0), q(8.0, -7.0), q(8.0, 7.0), q(-8.0, 7.0)], true, st);
            path(p, &[q(-2.7, -7.0), q(-2.7, 7.0)], false, st);
            path(p, &[q(2.7, -7.0), q(2.7, 7.0)], false, st);
        }
        Glyph::Trash => {
            path(p, &[q(-8.0, -5.0), q(8.0, -5.0)], false, st);
            path(p, &[q(-2.5, -5.0), q(-2.5, -8.0), q(2.5, -8.0), q(2.5, -5.0)], false, st);
            path(p, &[q(-6.0, -5.0), q(-5.0, 8.0), q(5.0, 8.0), q(6.0, -5.0)], false, st);
            path(p, &[q(-2.0, -1.5), q(-2.0, 4.5)], false, st);
            path(p, &[q(2.0, -1.5), q(2.0, 4.5)], false, st);
        }
        Glyph::Clock => {
            p.circle_stroke(q(0.0, 0.0), 8.0, st);
            path(p, &[q(0.0, -4.5), q(0.0, 0.0), q(4.0, 2.5)], false, st);
        }
        Glyph::Search => {
            p.circle_stroke(q(-1.5, -1.5), 6.5, st);
            path(p, &[q(3.5, 3.5), q(8.0, 8.0)], false, st);
        }
        Glyph::Check => {
            path(p, &[q(-8.0, -8.0), q(8.0, -8.0), q(8.0, 8.0), q(-8.0, 8.0)], true, st);
            path(p, &[q(-4.0, 0.5), q(-1.0, 4.0), q(4.5, -3.5)], false, st);
        }
    }
}

// ---------------------------------------------------------------------------
// Iconos de archivo
// ---------------------------------------------------------------------------

pub fn kind_color(k: Kind) -> Color32 {
    match k {
        Kind::Folder => Color32::from_rgb(244, 190, 80),
        Kind::Image => Color32::from_rgb(76, 175, 120),
        Kind::Video => Color32::from_rgb(226, 98, 98),
        Kind::Audio => Color32::from_rgb(170, 110, 225),
        Kind::Text => Color32::from_rgb(86, 156, 235),
        Kind::Archive => Color32::from_rgb(222, 150, 70),
        Kind::Doc => Color32::from_rgb(226, 120, 90),
        Kind::Other => Color32::from_rgb(130, 140, 160),
    }
}

pub fn draw_folder(p: &Painter, rect: Rect) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let w = s * 0.92;
    let h = s * 0.74;
    let body = Rect::from_center_size(c + vec2(0.0, s * 0.04), vec2(w, h));
    let rad = CornerRadius::same((s * 0.1) as u8);
    let tab = Rect::from_min_size(body.min + vec2(0.0, -s * 0.09), vec2(w * 0.42, s * 0.2));
    p.rect_filled(tab, rad, Color32::from_rgb(214, 150, 40));
    p.rect_filled(body, rad, Color32::from_rgb(222, 162, 52));
    let front = Rect::from_min_max(body.min + vec2(0.0, h * 0.2), body.max);
    p.rect_filled(front, rad, Color32::from_rgb(247, 196, 92));
}

pub fn draw_file(p: &Painter, rect: Rect, kind: Kind, ext: &str, pal: &Pal) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let w = s * 0.68;
    let h = s * 0.88;
    let page = Rect::from_center_size(c, vec2(w, h));
    let fold = w * 0.3;
    let col = kind_color(kind);
    let rad = (s * 0.07) as u8;
    p.rect_filled(page, CornerRadius { nw: rad, ne: 0, sw: rad, se: rad }, pal.page);
    // en temas claros la hoja es blanca sobre blanco: se le da un contorno que la separe del fondo
    if [pal.bg, pal.panel, pal.surface].iter().any(|g| contrast_ratio(pal.page, *g) < MIN_UI_CONTRAST) {
        p.rect_stroke(page, CornerRadius { nw: rad, ne: 0, sw: rad, se: rad }, Stroke::new(1.0, pal.outline), egui::StrokeKind::Inside);
    }
    // el glifo y la extensión informan del tipo: que se distingan de la hoja y de la banda
    let gcol = ensure_contrast(col, pal.page, MIN_UI_CONTRAST);
    // esquina doblada
    let tr = page.right_top();
    p.add(Shape::convex_polygon(
        vec![tr + vec2(-fold, 0.0), tr + vec2(0.0, fold), tr + vec2(-fold, fold)],
        col.gamma_multiply(0.55),
        Stroke::NONE,
    ));
    // banda de color con extensión
    let band = Rect::from_min_max(pos2(page.left(), page.bottom() - h * 0.26), page.right_bottom());
    p.rect_filled(band, CornerRadius { nw: 0, ne: 0, sw: rad, se: rad }, col);
    if s >= 52.0 && !ext.is_empty() {
        let t: String = ext.chars().take(4).collect::<String>().to_uppercase();
        p.text(band.center(), egui::Align2::CENTER_CENTER, t, egui::FontId::proportional((s * 0.13).clamp(8.0, 15.0)), on_color(col));
    }
    // glifo central
    let gc = pos2(c.x, page.top() + h * 0.42);
    let gs = w * 0.5;
    match kind {
        Kind::Image => draw_glyph(p, gc, gs, Glyph::Picture, gcol),
        Kind::Audio => draw_glyph(p, gc, gs, Glyph::Music, gcol),
        Kind::Video => draw_glyph(p, gc, gs, Glyph::Video, gcol),
        Kind::Doc | Kind::Text => draw_glyph(p, gc, gs, Glyph::Document, gcol),
        Kind::Archive => {
            let st = Stroke::new((s / 18.0).max(1.2), gcol);
            for i in 0..4 {
                let y = gc.y - gs * 0.45 + i as f32 * gs * 0.3;
                let x = gc.x + if i % 2 == 0 { -gs * 0.08 } else { gs * 0.08 };
                p.line_segment([pos2(x - gs * 0.12, y), pos2(x + gs * 0.12, y)], st);
            }
        }
        _ => {}
    }
}


// ---------------------------------------------------------------------------
// Diálogos modales: tarjeta con icono, texto y botones coherentes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// acción destructiva
    Danger,
    Warn,
    Info,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Btn {
    /// acción principal (acento)
    Primary,
    /// acción destructiva (rojo)
    Danger,
    Plain,
}

fn tone_color(p: &Pal, t: Tone) -> Color32 {
    match t {
        Tone::Danger => p.danger,
        Tone::Warn => p.warn,
        Tone::Info => p.accent,
    }
}

/// Alto de los botones de diálogo (más en pantallas táctiles).
fn dialog_button_height() -> f32 {
    min_target().max(34.0)
}

/// Tamaño que ocupará un botón de diálogo con ese texto.
fn dialog_button_size(ui: &egui::Ui, text: &str) -> egui::Vec2 {
    let g = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(14.0), Color32::WHITE);
    let h = dialog_button_height();
    vec2((g.size().x + 36.0).max(84.0).max(h), h)
}

/// Botón de diálogo: relleno según su función, esquinas redondas, realce al pasar el ratón y anillo de
/// foco con el teclado. El texto sobre relleno es blanco o negro según el contraste (`on_color`).
pub fn dialog_button(ui: &mut egui::Ui, p: &Pal, kind: Btn, text: &str) -> egui::Response {
    let font = egui::FontId::proportional(14.0);
    let enabled = ui.is_enabled();
    let (fill, fg, border) = match kind {
        Btn::Primary => (p.accent, p.on_accent, p.accent),
        Btn::Danger => (p.danger_fill, p.on_danger, p.danger_fill),
        Btn::Plain => (p.surface, p.text, p.outline),
    };
    let size = dialog_button_size(ui, text);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.4) };
    let (fill, fg, border) = (dim(fill), dim(fg), dim(border));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    // con texto blanco el realce oscurece y con texto negro aclara: el contraste nunca baja
    let white = fg == Color32::WHITE;
    let mut fill = fill;
    if resp.hovered() {
        fill = match kind {
            Btn::Plain => p.hover,
            _ if white => darken(fill, 0.10),
            _ => lighten(fill, 0.12),
        };
    }
    if resp.is_pointer_button_down_on() {
        fill = match kind {
            Btn::Plain => p.border,
            _ if white => darken(fill, 0.20),
            _ => lighten(fill, 0.24),
        };
    }
    ui.painter().rect_filled(rect, 10.0, fill);
    if kind == Btn::Plain {
        ui.painter().rect_stroke(rect, 10.0, Stroke::new(1.0, border), egui::StrokeKind::Inside);
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
    focus_ring(ui, p, &resp, 10.0);
    resp
}

fn lighten(c: Color32, k: f32) -> Color32 {
    let f = |x: u8| (x as f32 + (255.0 - x as f32) * k).round() as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

fn darken(c: Color32, k: f32) -> Color32 {
    let f = |x: u8| (x as f32 * (1.0 - k)).round() as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// Tarjeta modal centrada: velo sobre el resto de la interfaz, icono de tono, título, contenido y botones
/// alineados a la derecha (`actions` los añade de derecha a izquierda: el principal primero).
pub fn modal_card(
    ctx: &egui::Context,
    p: &Pal,
    tone: Tone,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut egui::Ui),
    actions: impl FnOnce(&mut egui::Ui),
) {
    // velo: oscurece y bloquea lo de debajo
    let screen = ctx.content_rect();
    let scrim = egui::LayerId::new(egui::Order::Middle, egui::Id::new("modal_scrim_paint"));
    ctx.layer_painter(scrim).rect_filled(screen, 0.0, Color32::from_black_alpha(110));
    egui::Area::new(egui::Id::new("modal_scrim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
        });
    let frame = egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(18))
        .shadow(ctx.global_style().visuals.window_shadow)
        .inner_margin(egui::Margin::same(22));
    let win = egui::Window::new(title)
        .title_bar(false)
        .frame(frame)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        // espacio de maquetación de toda la pantalla (si no, egui lo deja en ~420 pt); la tarjeta
        // pintada sigue midiendo lo que su contenido
        .min_height(dialog_max_outer_height(ctx))
        .max_height(dialog_max_outer_height(ctx))
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            // ancho limitado a la pantalla y cuerpo con desplazamiento si no cabe en alto: los botones
            // quedan siempre a la vista
            let width = dialog_width(ctx, width);
            let max_body = (screen.height() - 2.0 * DIALOG_GAP - 2.0 * 22.0 - 20.0 - dialog_button_height()).max(80.0);
            ui.set_width(width);
            ui.horizontal_top(|ui| {
                // insignia redonda con el glifo del tono
                let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), egui::Sense::hover());
                let c = tone_color(p, tone);
                ui.painter().circle_filled(r.center(), 22.0, Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 40));
                let glyph = match tone {
                    Tone::Danger | Tone::Warn => "!",
                    Tone::Info => "i",
                };
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(24.0), c);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    let w = (width - 64.0).max(80.0);
                    ui.set_width(w);
                    egui::ScrollArea::vertical().max_height(max_body).auto_shrink([false, true]).show(ui, |ui| {
                        ui.set_width(w - 8.0);
                        dialog_title(ui, title);
                        ui.add_space(4.0);
                        body(ui);
                    });
                });
            });
            ui.add_space(20.0);
            ui.allocate_ui_with_layout(vec2(width, dialog_button_height()), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                actions(ui);
            });
        });
    if let Some(w) = &win {
        trap_focus(ctx, w.response.layer_id);
    }
}

// ---------------------------------------------------------------------------
// Ventanas con tarjeta (Opciones…), pestañas en píldora e iconos de botón
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Close,
    Copy,
    Trash,
}

/// Botón cuadrado de solo icono (dibujado con trazos, sin depender de la fuente).
pub fn icon_button(ui: &mut egui::Ui, p: &Pal, icon: Icon, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(target_size(vec2(28.0, 28.0)), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tip));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
    if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, p.hover);
    }
    focus_ring(ui, p, &resp, 8.0);
    let col = if icon == Icon::Trash && resp.hovered() { p.danger } else if resp.hovered() { p.text } else { p.subtle };
    let st = Stroke::new(1.6, col);
    let c = rect.center();
    let pt = ui.painter();
    match icon {
        Icon::Close => {
            pt.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], st);
            pt.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], st);
        }
        Icon::Copy => {
            pt.rect_stroke(Rect::from_min_size(c + vec2(-2.0, -7.0), vec2(9.0, 9.0)), 2.0, st, egui::StrokeKind::Middle);
            pt.rect_stroke(Rect::from_min_size(c + vec2(-7.0, -2.0), vec2(9.0, 9.0)), 2.0, st, egui::StrokeKind::Middle);
        }
        Icon::Trash => {
            pt.line_segment([c + vec2(-6.0, -4.0), c + vec2(6.0, -4.0)], st);
            pt.line_segment([c + vec2(-2.0, -6.5), c + vec2(2.0, -6.5)], st);
            pt.rect_stroke(Rect::from_min_max(c + vec2(-4.5, -4.0), c + vec2(4.5, 7.0)), 2.0, st, egui::StrokeKind::Middle);
        }
    }
    resp
}

/// Pestañas en píldora; devuelve true si cambió la selección.
pub fn tab_bar(ui: &mut egui::Ui, p: &Pal, sel: &mut u8, labels: &[&str]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, l) in labels.iter().enumerate() {
            let font = egui::FontId::proportional(14.0);
            let active = *sel == i as u8;
            let col = if active { p.accent } else { p.subtle };
            let g = ui.painter().layout_no_wrap(l.to_string(), font, col);
            let h = min_target().max(30.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 28.0, h), egui::Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, *l));
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            let rad = h / 2.0;
            if active {
                // el relleno de selección es suave: el contorno es el indicador de estado (≥ 3:1)
                ui.painter().rect_filled(rect, rad, p.sel);
                ui.painter().rect_stroke(rect, rad, Stroke::new(1.0, p.focus), egui::StrokeKind::Inside);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, rad, p.hover);
            }
            ui.painter().galley(rect.center() - g.size() / 2.0, g, col);
            focus_ring(ui, p, &resp, rad);
            if resp.clicked() && !active {
                *sel = i as u8;
                changed = true;
            }
        }
    });
    changed
}

/// Encabezado de sección: texto pequeño y atenuado.
pub fn section(ui: &mut egui::Ui, p: &Pal, text: &str) {
    let _ = section_r(ui, p, text);
}

/// Como `section`, y devuelve el título para nombrar con él el control que sigue (`labelled_by`).
pub fn section_r(ui: &mut egui::Ui, p: &Pal, text: &str) -> egui::Response {
    ui.add_space(10.0);
    let r = ui.label(egui::RichText::new(text.to_uppercase()).size(11.0).strong().color(p.subtle));
    ui.add_space(2.0);
    r
}

// ---------------------------------------------------------------------------
// Diálogos: medidas, título, fila de botones y tarjeta con pie fijo
// ---------------------------------------------------------------------------

/// Margen mínimo entre un diálogo y el borde de la pantalla.
pub const DIALOG_GAP: f32 = 16.0;
/// Márgenes interiores más anchos de una tarjeta de diálogo (`modal_card`: 22 a cada lado).
const DIALOG_CHROME_X: f32 = 2.0 * 22.0 + 2.0;

/// Ancho de contenido de un diálogo: el pedido, limitado a lo que cabe en pantalla (márgenes de la
/// tarjeta y `DIALOG_GAP` a cada lado). Usar en vez de anchos fijos (`set_width(520.0)`…).
pub fn dialog_width(ctx: &egui::Context, wanted: f32) -> f32 {
    let avail = (ctx.content_rect().width() - DIALOG_CHROME_X - 2.0 * DIALOG_GAP).max(0.0);
    wanted.min(avail).max(avail.min(200.0))
}

/// Alto máximo exterior de un diálogo (pantalla menos `DIALOG_GAP` arriba y abajo).
pub fn dialog_max_outer_height(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - 2.0 * DIALOG_GAP).max(120.0)
}

/// Título de diálogo (mismo estilo en todas las familias de diálogos).
pub fn dialog_title(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).size(18.0).strong());
}

/// Cabecera de diálogo: título a la izquierda y, si se pasa `close`, botón de cerrar a la derecha.
pub fn dialog_header(ui: &mut egui::Ui, p: &Pal, title: &str, close: Option<&mut bool>) {
    ui.horizontal(|ui| {
        dialog_title(ui, title);
        if let Some(open) = close {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, p, Icon::Close, crate::i18n::t("Cerrar")).clicked() {
                    *open = false;
                }
            });
        }
    });
}

fn popup_start_id() -> egui::Id {
    egui::Id::new("fb_popup_at_frame_start")
}

/// Llamar al empezar cada fotograma: recuerda si había un menú o desplegable abierto. egui lo cierra
/// con Esc durante el fotograma (sin consumir la tecla) y ese Esc no debe cerrar también el diálogo.
pub fn note_frame_start(ctx: &egui::Context) {
    let open = egui::Popup::is_any_open(ctx);
    ctx.data_mut(|d| d.insert_temp(popup_start_id(), open));
}

/// ¿Es de un desplegable el Esc de este fotograma? (había uno abierto al empezar o lo hay ahora)
pub fn popup_was_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(popup_start_id())).unwrap_or(false) || egui::Popup::is_any_open(ctx)
}

/// Esc para cerrar un diálogo: pulsado y no es de un desplegable abierto.
pub fn dialog_esc(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !popup_was_open(ctx)
}

/// ¿Primer fotograma del diálogo `id`? (no se dibujó en el anterior). Llamar en cada fotograma en que
/// se muestra: la tecla con la que se abrió no debe contestarlo.
pub fn dialog_fresh(ctx: &egui::Context, id: egui::Id) -> bool {
    let id = id.with("fb_dialog_fresh");
    let frame = ctx.cumulative_frame_nr();
    let last = ctx.data(|d| d.get_temp::<u64>(id));
    ctx.data_mut(|d| d.insert_temp(id, frame));
    !matches!(last, Some(l) if l.saturating_add(1) >= frame)
}

/// Encierra el foco del teclado (y los clics) en la capa del diálogo: Tab no sale a lo de detrás.
pub fn trap_focus(ctx: &egui::Context, layer: egui::LayerId) {
    ctx.memory_mut(|m| m.set_modal_layer(layer));
}

/// ¿Tiene el foco un campo de texto? (Esc entonces es del campo, no del diálogo.)
fn text_edit_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused()).is_some_and(|id| egui::text_edit::TextEditState::load(ctx, id).is_some())
}

/// Resultado de una fila de botones de diálogo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogAction {
    None,
    /// botón seguro (Cancelar/Cerrar) o Esc
    Cancel,
    /// botón intermedio `i` de `extras`
    Extra(usize),
    /// acción principal
    Primary,
}

/// Fila de botones con una única convención para todos los diálogos: alineada a la derecha,
/// Cancelar primero, los intermedios después y la acción principal la última (a la derecha);
/// una principal destructiva va en rojo. Esc equivale a Cancelar (salvo dentro de un campo de texto) y,
/// con `focus_safe`, al abrirse el diálogo el foco del teclado empieza en el botón seguro.
pub struct DialogButtons<'a> {
    cancel: Option<&'a str>,
    extras: &'a [&'a str],
    primary: Option<(&'a str, Btn)>,
    primary_enabled: bool,
    focus_safe: bool,
}

impl<'a> DialogButtons<'a> {
    /// «Cancelar | Principal».
    pub fn new(cancel: &'a str, primary: &'a str) -> Self {
        DialogButtons { cancel: Some(cancel), extras: &[], primary: Some((primary, Btn::Primary)), primary_enabled: true, focus_safe: false }
    }
    /// Un solo botón para cerrar (diálogos informativos).
    pub fn close_only(close: &'a str) -> Self {
        DialogButtons { cancel: Some(close), extras: &[], primary: None, primary_enabled: true, focus_safe: false }
    }
    /// La acción principal es destructiva: botón rojo y foco inicial en el botón seguro.
    pub fn danger(mut self) -> Self {
        if let Some((t, _)) = self.primary {
            self.primary = Some((t, Btn::Danger));
        }
        self.focus_safe = true;
        self
    }
    /// Habilita o deshabilita la acción principal (p. ej. hasta que el formulario sea válido).
    pub fn primary_enabled(mut self, on: bool) -> Self {
        self.primary_enabled = on;
        self
    }
    /// Botones intermedios, entre Cancelar y la principal (p. ej. «Omitir», «Aplicar»).
    pub fn extras(mut self, extras: &'a [&'a str]) -> Self {
        self.extras = extras;
        self
    }
    /// Pone el foco inicial en el botón seguro (Cancelar).
    pub fn focus_safe(mut self, on: bool) -> Self {
        self.focus_safe = on;
        self
    }

    pub fn show(self, ui: &mut egui::Ui, p: &Pal) -> DialogAction {
        let ctx = ui.ctx().clone();
        let mut items: Vec<(&str, Btn, bool, DialogAction)> = Vec::new();
        if let Some(c) = self.cancel {
            items.push((c, Btn::Plain, true, DialogAction::Cancel));
        }
        for (i, e) in self.extras.iter().enumerate() {
            items.push((e, Btn::Plain, true, DialogAction::Extra(i)));
        }
        if let Some((t, k)) = self.primary {
            items.push((t, k, self.primary_enabled, DialogAction::Primary));
        }
        // ¿primer fotograma de este diálogo? (la fila no se dibujó en el anterior)
        let row_id = ui.id().with("dialog_buttons");
        let frame = ctx.cumulative_frame_nr();
        let last = ctx.data(|d| d.get_temp::<u64>(row_id));
        ctx.data_mut(|d| d.insert_temp(row_id, frame));
        let fresh = !matches!(last, Some(l) if l.saturating_add(1) >= frame);
        let sp = 10.0;
        let total: f32 = items.iter().map(|(t, ..)| dialog_button_size(ui, t).x).sum::<f32>() + sp * items.len().saturating_sub(1) as f32;
        let avail = ui.available_width();
        let mut act = DialogAction::None;
        let mut add = |ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing.x = sp;
            for (t, k, en, a) in &items {
                let r = ui.add_enabled_ui(*en, |ui| dialog_button(ui, p, *k, t)).inner;
                if r.clicked() {
                    act = *a;
                }
                if fresh && self.focus_safe && *a == DialogAction::Cancel {
                    r.request_focus();
                }
            }
        };
        if total <= avail - 1.0 {
            ui.horizontal(|ui| {
                ui.add_space(avail - total - 1.0);
                add(ui);
            });
        } else {
            // no cabe en una línea (pantalla estrecha): se reparte en varias, en el mismo orden
            ui.horizontal_wrapped(|ui| add(ui));
        }
        if self.cancel.is_some() && act == DialogAction::None && !text_edit_focused(&ctx) && !popup_was_open(&ctx) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            act = DialogAction::Cancel;
        }
        act
    }
}

/// Atajo de `DialogButtons` para el caso habitual «Cancelar | Principal».
pub fn button_row(ui: &mut egui::Ui, p: &Pal, cancel: &str, primary: &str, kind: Btn, primary_enabled: bool) -> DialogAction {
    let b = DialogButtons::new(cancel, primary).primary_enabled(primary_enabled);
    let b = if kind == Btn::Danger { b.danger() } else { b };
    b.show(ui, p)
}

/// Ventana centrada con tarjeta redondeada y cabecera (título + cerrar). `open` pasa a false al cerrarla.
/// El contenido usa todo el alto de la pantalla antes de desplazarse.
pub fn card_window(ctx: &egui::Context, p: &Pal, title: &str, width: f32, open: &mut bool, body: impl FnOnce(&mut egui::Ui)) {
    card_impl::<(), fn(&mut egui::Ui)>(ctx, p, title, width, open, body, None);
}

/// Como `card_window`, con un pie fijo (normalmente `DialogButtons::show`) fuera de la zona con
/// desplazamiento: los botones quedan siempre a la vista aunque el contenido no quepa.
/// Devuelve lo que devuelva `footer` (None si la ventana no se dibujó).
pub fn card_window_with_footer<R>(
    ctx: &egui::Context,
    p: &Pal,
    title: &str,
    width: f32,
    open: &mut bool,
    body: impl FnOnce(&mut egui::Ui),
    footer: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    card_impl(ctx, p, title, width, open, body, Some(footer))
}

fn card_impl<R, F: FnOnce(&mut egui::Ui) -> R>(
    ctx: &egui::Context,
    p: &Pal,
    title: &str,
    width: f32,
    open: &mut bool,
    body: impl FnOnce(&mut egui::Ui),
    footer: Option<F>,
) -> Option<R> {
    // en pantallas estrechas (móvil) la tarjeta se ajusta al ancho disponible y el contenido se desplaza
    let width = dialog_width(ctx, width);
    let outer = dialog_max_outer_height(ctx);
    let foot_id = egui::Id::new(("card_footer_h", title));
    let foot_h = if footer.is_some() { ctx.data(|d| d.get_temp::<f32>(foot_id)).unwrap_or(dialog_button_height() + 14.0) } else { 0.0 };
    // alto del cuerpo: el exterior menos márgenes (2 × 20), cabecera (botón de cerrar + separación) y pie
    let header_h = min_target().max(28.0) + 10.0 + 2.0 * 6.0;
    let max_h = (outer - 2.0 * 20.0 - 2.0 - header_h - foot_h).max(80.0);
    let frame = egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(16))
        .shadow(ctx.global_style().visuals.window_shadow)
        .inner_margin(egui::Margin::same(20));
    let mut out = None;
    let win = egui::Window::new(title)
        .title_bar(false)
        .frame(frame)
        .collapsible(false)
        .resizable(false)
        // espacio de maquetación de toda la pantalla (si no, egui lo deja en ~420 pt y el contenido se
        // desplaza aunque quepa); la tarjeta pintada sigue midiendo lo que su contenido
        .min_height(outer)
        .max_height(outer)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_width(width);
            dialog_header(ui, p, title, Some(&mut *open));
            ui.add_space(10.0);
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.set_width(width - 8.0);
                body(ui);
            });
            if let Some(f) = footer {
                let top = ui.min_rect().bottom();
                ui.add_space(14.0);
                out = Some(f(ui));
                let h = ui.min_rect().bottom() - top;
                if h.is_finite() && h > 0.0 {
                    ctx.data_mut(|d| d.insert_temp(foot_id, h));
                }
            }
        });
    if let Some(w) = &win {
        trap_focus(ctx, w.response.layer_id);
    }
    if dialog_esc(ctx) && !text_edit_focused(ctx) {
        *open = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Los dos temas de serie y todos los predefinidos, con su nombre.
    fn all_themes() -> Vec<(String, Pal)> {
        let mut v = vec![("Claro".to_string(), palette(false)), ("Oscuro".to_string(), palette(true))];
        v.extend(presets().iter().map(|t| (t.name.clone(), palette_of(t))));
        v
    }

    fn check(name: &str, what: &str, fg: Color32, bg: Color32, min: f32) {
        let r = contrast_ratio(fg, bg);
        assert!(r >= min, "{name}: {what} {fg:?} sobre {bg:?} = {r:.2} < {min}");
    }

    #[test]
    fn contraste_basico() {
        assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.001);
        assert_eq!(on_color(Color32::WHITE), Color32::BLACK);
        assert_eq!(on_color(Color32::BLACK), Color32::WHITE);
        // cualquier relleno da ≥ 4.5:1 con su texto
        for r in (0..=255u32).step_by(15) {
            for g in (0..=255u32).step_by(15) {
                for b in (0..=255u32).step_by(15) {
                    let c = Color32::from_rgb(r as u8, g as u8, b as u8);
                    check("rejilla", "on_color", on_color(c), c, MIN_TEXT_CONTRAST);
                }
            }
        }
        let c = ensure_contrast(Color32::from_rgb(130, 130, 130), Color32::WHITE, 4.5);
        assert!(contrast_ratio(c, Color32::WHITE) >= 4.5);
        // si ya cumple, no se toca
        assert_eq!(ensure_contrast(Color32::BLACK, Color32::WHITE, 4.5), Color32::BLACK);
    }

    #[test]
    fn todos_los_temas_cumplen_wcag() {
        for (name, p) in all_themes() {
            let n = name.as_str();
            let sels = [over(p.sel, p.bg), over(p.sel, p.panel), over(p.sel, p.surface)];
            for g in [p.bg, p.panel, p.surface, p.hover].into_iter().chain(sels) {
                check(n, "texto", p.text, g, MIN_TEXT_CONTRAST);
                check(n, "texto secundario", p.subtle, g, MIN_TEXT_CONTRAST);
            }
            for g in [p.bg, p.panel, p.surface].into_iter().chain(sels) {
                check(n, "acento como texto", p.accent, g, MIN_TEXT_CONTRAST);
            }
            for g in [p.bg, p.panel, p.surface, p.hover] {
                check(n, "error", p.danger, g, MIN_TEXT_CONTRAST);
                check(n, "aviso", p.warn, g, MIN_TEXT_CONTRAST);
                check(n, "foco / indicador de selección", p.focus, g, MIN_UI_CONTRAST);
                check(n, "icono inactivo", p.disabled, g, MIN_UI_CONTRAST);
            }
            for g in [p.bg, p.panel, p.surface] {
                check(n, "contorno de controles", p.outline, g, MIN_UI_CONTRAST);
            }
            check(n, "texto del botón principal", p.on_accent, p.accent, MIN_TEXT_CONTRAST);
            check(n, "texto del botón destructivo", p.on_danger, p.danger_fill, MIN_TEXT_CONTRAST);
            // los realces del botón no bajan el contraste del texto
            let (h, d) = if p.on_accent == Color32::WHITE { (darken(p.accent, 0.10), darken(p.accent, 0.20)) } else { (lighten(p.accent, 0.12), lighten(p.accent, 0.24)) };
            check(n, "texto del principal (hover)", p.on_accent, h, MIN_TEXT_CONTRAST);
            check(n, "texto del principal (pulsado)", p.on_accent, d, MIN_TEXT_CONTRAST);
            check(n, "texto del destructivo (hover)", p.on_danger, darken(p.danger_fill, 0.10), MIN_TEXT_CONTRAST);
            // iconos de archivo: glifo distinguible de la hoja y extensión legible sobre la banda
            for k in [Kind::Image, Kind::Video, Kind::Audio, Kind::Text, Kind::Archive, Kind::Doc, Kind::Other] {
                check(n, "glifo de archivo", ensure_contrast(kind_color(k), p.page, MIN_UI_CONTRAST), p.page, MIN_UI_CONTRAST);
                check(n, "extensión sobre banda", on_color(kind_color(k)), kind_color(k), MIN_TEXT_CONTRAST);
            }
        }
    }

    #[test]
    fn temas_propios_raros_dan_texto_legible_sobre_el_acento() {
        // acentos claros, oscuros y medios en temas claros y oscuros
        for (dark, bg) in [(true, [20u8, 20, 20]), (false, [250, 250, 250])] {
            for ac in [[255u8, 255, 0], [0, 0, 80], [128, 128, 128], [166, 226, 46], [250, 189, 47], [255, 255, 255], [0, 0, 0]] {
                let th = crate::config::CustomTheme { name: "x".into(), dark, colors: [bg, bg, bg, bg, bg, [128, 128, 128], [128, 128, 128], ac, bg] };
                let p = palette_of(&th);
                check("propio", "texto sobre acento", p.on_accent, p.accent, MIN_TEXT_CONTRAST);
                check("propio", "texto", p.text, p.bg, MIN_TEXT_CONTRAST);
            }
        }
    }

    #[test]
    fn la_derivacion_es_estable() {
        // volver a derivar una paleta ya derivada (p. ej. al duplicar el tema activo) no la cambia
        for (name, p) in all_themes() {
            let dark = luminance(p.bg) < 0.2;
            let again = from_colors(p.colors(), dark);
            assert_eq!(again.colors(), p.colors(), "{name}");
        }
    }

    #[test]
    fn el_movimiento_reducido_salta_la_transicion() {
        let (a, b) = (palette(false), palette(true));
        set_reduced_motion(true);
        assert_eq!(a.lerp(&b, 0.1).colors(), b.colors());
        set_reduced_motion(false);
        assert_ne!(a.lerp(&b, 0.1).colors(), b.colors());
    }

    #[test]
    fn tamanos_minimos() {
        assert!(MIN_TARGET >= 24.0 && MIN_TARGET_TOUCH >= 44.0);
        assert!(target_size(vec2(10.0, 30.0)).x >= MIN_TARGET);
        assert!(dialog_button_height() >= min_target());
    }
}
