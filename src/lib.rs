//! Gestor de archivos ligero (escritorio y Android).
pub mod app;
pub mod archive;
pub mod audio;
pub mod index;
pub mod commands;
pub mod contentsearch;
pub mod diskusage;
pub mod docview;
pub mod comic;
pub mod exif;
pub mod meta;
pub mod fontview;
pub mod tree;
pub mod sqlite;
pub mod table;
pub mod rich;
pub mod markup;
pub mod imgx;
pub mod dragout;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
mod dragout_linux;
#[cfg(windows)]
mod dragout_windows;
#[cfg(target_os = "macos")]
mod dragout_macos;
pub mod config;
pub mod extras;
pub mod fsys;
pub mod gpu3d;
pub mod pbr;
#[cfg(test)]
mod fuzz;
pub mod i18n;
pub mod mesh3d;
pub mod netmount;
pub mod i18n_table;
pub mod preview;
pub mod sftp;
pub mod sysicons;
pub mod system;
pub mod terminal;
pub mod theme;
pub mod tools;
#[cfg(not(target_os = "android"))]
pub mod video;
#[cfg(target_os = "android")]
#[path = "droid_video.rs"]
pub mod video;
#[cfg(target_os = "android")]
pub mod droid;
pub mod thumbs;


fn window_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/icon-256.png")).expect("icono").into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width, height }
}

fn options(cfg: &config::Config) -> eframe::NativeOptions {
    let mut vb = egui::ViewportBuilder::default()
        .with_title("File Browser")
        .with_decorations(cfg.native_deco)
        .with_transparent(!cfg.native_deco)
        // dentro de Flatpak el app_id debe coincidir con el ID de la aplicación
        .with_app_id(std::env::var("FLATPAK_ID").unwrap_or_else(|_| "filebrowser-rs".into()))
        .with_icon(window_icon())
        .with_inner_size([cfg.win.0, cfg.win.1])
        .with_min_inner_size([640.0, 420.0]);
    if let Some((x, y)) = std::env::var("FB_POS").ok().and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?))
    }) {
        vb = vb.with_position([x, y]);
    }
    // búfer de profundidad: lo necesita el visor 3D por GPU
    let mut o = eframe::NativeOptions { viewport: vb, depth_buffer: 24, ..Default::default() };
    // En Wayland, eglSwapBuffers con vsync espera al aviso del compositor, que no llega mientras la
    // ventana principal está tapada o en otro escritorio: el bucle entero se queda parado ahí y el
    // visor (una ventana aparte, dibujada dentro del fotograma de la principal) deja de actualizarse
    // aunque el audio siga sonando. Sin vsync el intercambio no bloquea; Wayland no produce
    // «tearing», así que no se pierde nada.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        o.glow_options.vsync = false;
    }
    o
}

/// Instante de arranque del proceso (para la traza de rendimiento de FB_SHOT).
pub static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn run_with(opts: eframe::NativeOptions, cfg: config::Config) -> eframe::Result {
    if std::env::var_os("FB_SHOT").is_some() {
        eprintln!("[trace] antes de eframe: {:?}", T0.get().map(|t| t.elapsed()));
    }
    eframe::run_native("File Browser", opts, Box::new(move |cc| {
            let tr = std::env::var_os("FB_SHOT").is_some();
            if tr {
                eprintln!("[trace] contexto GL listo: {:?}", T0.get().map(|t| t.elapsed()));
            }
            let a = app::App::new(cc, cfg);
            if tr {
                eprintln!("[trace] App::new listo: {:?}", T0.get().map(|t| t.elapsed()));
            }
            Ok(Box::new(a))
        }))
}

/// Punto de entrada de escritorio.
#[cfg(not(target_os = "android"))]
pub fn run() -> eframe::Result {
    let _ = T0.set(std::time::Instant::now());
    // `ssh` nos llama como programa de contraseñas (SSH_ASKPASS) para conectar por SFTP
    sftp::askpass_main();
    if let system::Cli::Exit(code) = system::start() {
        std::process::exit(code);
    }
    let cfg = config::Config::load();
    run_with(options(&cfg), cfg)
}

/// Punto de entrada de Android (NativeActivity).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    // en Android no hay HOME ni XDG: usamos el almacenamiento compartido y la carpeta de datos de la app
    if let Some(p) = app.internal_data_path() {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", p) };
    }
    unsafe { std::env::set_var("HOME", "/storage/emulated/0") };
    let _ = ANDROID.set((app.vm_as_ptr() as usize, app.activity_as_ptr() as usize));
    immersive(&app);
    allow_file_uris();
    let cfg = config::Config::load();
    let mut opts = options(&cfg);
    opts.android_app = Some(app);
    let _ = run_with(opts, cfg);
}

/// Android no tiene papelera: el borrado a papelera devuelve error en lugar de borrar.
#[cfg(target_os = "android")]
pub mod trash {
    pub fn delete<P: AsRef<std::path::Path>>(_p: P) -> Result<(), String> {
        Err("la papelera no está disponible en Android".into())
    }
}

/// Pantalla completa inmersiva: oculta barra de estado y de navegación y usa la zona del notch.
#[cfg(target_os = "android")]
fn immersive(app: &android_activity::AndroidApp) {
    use jni::objects::{JObject, JValue};
    let go = || -> jni::errors::Result<()> {
        let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut _)? };
        let mut env = vm.attach_current_thread()?;
        let act = unsafe { JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
        let win = env.call_method(&act, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
        let decor = env.call_method(&win, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
        // LAYOUT_STABLE | LAYOUT_HIDE_NAVIGATION | LAYOUT_FULLSCREEN | HIDE_NAVIGATION | FULLSCREEN | IMMERSIVE_STICKY
        env.call_method(&decor, "setSystemUiVisibility", "(I)V", &[JValue::Int(0x1706)])?;
        // contenido también bajo el recorte de cámara (notch)
        let attrs = env.call_method(&win, "getAttributes", "()Landroid/view/WindowManager$LayoutParams;", &[])?.l()?;
        let _ = env.set_field(&attrs, "layoutInDisplayCutoutMode", "I", JValue::Int(1));
        let _ = env.call_method(&win, "setAttributes", "(Landroid/view/WindowManager$LayoutParams;)V", &[JValue::Object(&attrs)]);
        Ok(())
    };
    if go().is_err() {
        eprintln!("aviso: no se pudo activar el modo inmersivo");
    }
}

#[cfg(target_os = "android")]
static ANDROID: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();

/// Márgenes seguros del sistema en píxeles (izquierda, arriba, derecha, abajo): recorte de la
/// cámara, barras del sistema y esquinas. Cero fuera de Android o si aún no hay ventana.
#[cfg(target_os = "android")]
pub fn android_insets() -> [f32; 4] {
    use jni::objects::JObject;
    let Some(&(vm, act)) = ANDROID.get() else { return [0.0; 4] };
    let go = || -> jni::errors::Result<[f32; 4]> {
        let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _)? };
        let mut env = vm.attach_current_thread()?;
        let act = unsafe { JObject::from_raw(act as jni::sys::jobject) };
        let win = env.call_method(&act, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
        let decor = env.call_method(&win, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
        let wi = env.call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[])?.l()?;
        if wi.is_null() {
            return Ok([0.0; 4]);
        }
        let mut out = [0f32; 4];
        for (i, (sys, cut)) in [
            ("getSystemWindowInsetLeft", "getSafeInsetLeft"),
            ("getSystemWindowInsetTop", "getSafeInsetTop"),
            ("getSystemWindowInsetRight", "getSafeInsetRight"),
            ("getSystemWindowInsetBottom", "getSafeInsetBottom"),
        ]
        .into_iter()
        .enumerate()
        {
            let a = env.call_method(&wi, sys, "()I", &[])?.i()?;
            let mut b = 0;
            let dc = env.call_method(&wi, "getDisplayCutout", "()Landroid/view/DisplayCutout;", &[])?.l()?;
            if !dc.is_null() {
                b = env.call_method(&dc, cut, "()I", &[])?.i()?;
            }
            out[i] = a.max(b) as f32;
        }
        Ok(out)
    };
    go().unwrap_or_else(|_| {
        if let Ok(vm) = unsafe { jni::JavaVM::from_raw(vm as *mut _) } {
            if let Ok(mut env) = vm.attach_current_thread() {
                let _ = env.exception_clear();
            }
        }
        [0.0; 4]
    })
}

#[cfg(not(target_os = "android"))]
pub fn android_insets() -> [f32; 4] {
    [0.0; 4]
}

/// ¿Tiene la app el acceso a todos los archivos? Sin él Android sólo deja ver carpetas y medios.
#[cfg(target_os = "android")]
pub fn android_all_files() -> bool {
    let Some(&(vm, _)) = ANDROID.get() else { return true };
    let go = || -> jni::errors::Result<bool> {
        let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _)? };
        let mut env = vm.attach_current_thread()?;
        let sdk = env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()?;
        if sdk < 30 {
            return Ok(true);
        }
        env.call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?.z()
    };
    go().unwrap_or(true)
}

#[cfg(not(target_os = "android"))]
pub fn android_all_files() -> bool {
    true
}

/// Abre la pantalla de ajustes donde se concede el acceso a todos los archivos.
#[cfg(target_os = "android")]
pub fn android_request_all_files() {
    use jni::objects::{JObject, JValue};
    let Some(&(vm, act)) = ANDROID.get() else { return };
    let go = || -> jni::errors::Result<()> {
        let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _)? };
        let mut env = vm.attach_current_thread()?;
        let act = unsafe { JObject::from_raw(act as jni::sys::jobject) };
        let pkg = env.call_method(&act, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
        let pkg: String = env.get_string(&pkg.into())?.into();
        let uri_s = env.new_string(format!("package:{pkg}"))?;
        let uri = env
            .call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&uri_s)])?
            .l()?;
        let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        env.call_method(&act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    };
    if go().is_err() {
        if let Ok(vm) = unsafe { jni::JavaVM::from_raw(vm as *mut _) } {
            if let Ok(mut env) = vm.attach_current_thread() {
                let _ = env.exception_clear();
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn android_request_all_files() {}

/// Desactiva la comprobación de `file://` (StrictMode) para poder entregar archivos a otras apps.
#[cfg(target_os = "android")]
fn allow_file_uris() {
    let Some(&(vm, _)) = ANDROID.get() else { return };
    let go = || -> jni::errors::Result<()> {
        let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _)? };
        let mut env = vm.attach_current_thread()?;
        let b = env.new_object("android/os/StrictMode$VmPolicy$Builder", "()V", &[])?;
        let pol = env
            .call_method(&b, "build", "()Landroid/os/StrictMode$VmPolicy;", &[])?
            .l()?;
        env.call_static_method(
            "android/os/StrictMode",
            "setVmPolicy",
            "(Landroid/os/StrictMode$VmPolicy;)V",
            &[jni::objects::JValue::Object(&pol)],
        )?;
        Ok(())
    };
    if go().is_err() {
        eprintln!("aviso: no se pudo ajustar StrictMode");
    }
}

#[cfg(target_os = "android")]
fn android_mime(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        Some("mov") => "video/quicktime",
        Some("mp3") => "audio/mpeg",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/x-wav",
        Some("m4a") => "audio/mp4",
        Some("pdf") => "application/pdf",
        Some("txt" | "md" | "log" | "csv") => "text/plain",
        Some("html" | "htm") => "text/html",
        Some("json") => "application/json",
        Some("xml") => "text/xml",
        Some("zip") => "application/zip",
        Some("apk") => "application/vnd.android.package-archive",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("odt") => "application/vnd.oasis.opendocument.text",
        _ => "*/*",
    }
}

/// Abre un archivo con otra app: muestra siempre el selector «Abrir con».
#[cfg(target_os = "android")]
pub fn android_open(path: &std::path::Path) -> Result<(), String> {
    use jni::objects::{JObject, JValue};
    let Some(&(vm, act)) = ANDROID.get() else { return Err("sin actividad".into()) };
    let mime = android_mime(path);
    let p = path.to_string_lossy().into_owned();
    let go = || -> jni::errors::Result<()> {
        let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _)? };
        let mut env = vm.attach_current_thread()?;
        let act = unsafe { JObject::from_raw(act as jni::sys::jobject) };
        let jp = env.new_string(&p)?;
        let file = env.new_object("java/io/File", "(Ljava/lang/String;)V", &[JValue::Object(&jp)])?;
        let uri = env
            .call_static_method("android/net/Uri", "fromFile", "(Ljava/io/File;)Landroid/net/Uri;", &[JValue::Object(&file)])?
            .l()?;
        // un APK se entrega al instalador de paquetes: sólo acepta content://, así que usamos el id
        // del archivo en MediaStore (VIEW con file:// lo captaría cualquier app con filtro */*)
        let apk = mime == "application/vnd.android.package-archive";
        let mut uri = uri;
        let mut action_name = "android.intent.action.VIEW";
        if apk {
            let resolver = env
                .call_method(&act, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?
                .l()?;
            let base_s = env.new_string("content://media/external/file")?;
            let base = env
                .call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&base_s)])?
                .l()?;
            let col = env.new_string("_id")?;
            let string_cls = env.find_class("java/lang/String")?;
            let proj = env.new_object_array(1, &string_cls, &col)?;
            let sel = env.new_string("_data=?")?;
            let args = env.new_object_array(1, &string_cls, &jp)?;
            let null = JObject::null();
            let cur = env
                .call_method(
                    &resolver,
                    "query",
                    "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
                    &[
                        JValue::Object(&base),
                        JValue::Object(&proj),
                        JValue::Object(&sel),
                        JValue::Object(&args),
                        JValue::Object(&null),
                    ],
                )?
                .l()?;
            if !cur.is_null() {
                if env.call_method(&cur, "moveToFirst", "()Z", &[])?.z()? {
                    let id = env.call_method(&cur, "getLong", "(I)J", &[JValue::Int(0)])?.j()?;
                    uri = env
                        .call_static_method(
                            "android/content/ContentUris",
                            "withAppendedId",
                            "(Landroid/net/Uri;J)Landroid/net/Uri;",
                            &[JValue::Object(&base), JValue::Long(id)],
                        )?
                        .l()?;
                    action_name = "android.intent.action.INSTALL_PACKAGE";
                }
                env.call_method(&cur, "close", "()V", &[])?;
            }
        }
        let action = env.new_string(action_name)?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        let jm = env.new_string(mime)?;
        env.call_method(
            &intent,
            "setDataAndType",
            "(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&uri), JValue::Object(&jm)],
        )?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(0x1000_0001)])?;
        if action_name.ends_with("INSTALL_PACKAGE") {
            env.call_method(&act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
            return Ok(());
        }
        let title = env.new_string("Abrir con")?;
        let chooser = env
            .call_static_method(
                "android/content/Intent",
                "createChooser",
                "(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;",
                &[JValue::Object(&intent), JValue::Object(&title)],
            )?
            .l()?;
        env.call_method(&act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&chooser)])?;
        Ok(())
    };
    go().map_err(|e| {
        // limpiamos la excepción pendiente para que la app no caiga
        if let Ok(vm) = unsafe { jni::JavaVM::from_raw(vm as *mut _) } {
            if let Ok(mut env) = vm.attach_current_thread() {
                let _ = env.exception_clear();
            }
        }
        e.to_string()
    })
}
