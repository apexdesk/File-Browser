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
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
mod wl_activate;
#[cfg(windows)]
mod dragout_windows;
#[cfg(target_os = "macos")]
mod dragout_macos;
pub mod config;
pub mod extras;
pub mod sysapps;
pub mod systerm;
pub mod fsys;
pub mod vfs;
#[cfg(not(target_arch = "wasm32"))]
pub mod gpu3d;
#[cfg(target_arch = "wasm32")]
#[path = "gpu3d_web.rs"]
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
pub mod favs;
pub mod foldericons;
#[cfg(windows)]
pub mod winshell;
pub mod system;
#[cfg(not(target_arch = "wasm32"))]
pub mod terminal;
#[cfg(target_arch = "wasm32")]
#[path = "terminal_web.rs"]
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


#[cfg(not(target_arch = "wasm32"))]
fn window_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/icon-256.png")).expect("icono").into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width, height }
}

#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(feature = "selftest")]
pub static T0: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();

#[cfg(not(target_arch = "wasm32"))]
fn run_with(opts: eframe::NativeOptions, cfg: config::Config) -> eframe::Result {
    // desde el principio: un pánico fuera de las zonas protegidas (`thumbs::guarded`) aborta, como siempre
    thumbs::install_panic_hook();
    #[cfg(feature = "selftest")]
    if std::env::var_os("FB_SHOT").is_some() {
        eprintln!("[trace] antes de eframe: {:?}", T0.get().map(|t| t.elapsed()));
    }
    eframe::run_native("File Browser", opts, Box::new(move |cc| {
            #[cfg(feature = "selftest")]
            let tr = std::env::var_os("FB_SHOT").is_some();
            #[cfg(feature = "selftest")]
            if tr {
                eprintln!("[trace] contexto GL listo: {:?}", T0.get().map(|t| t.elapsed()));
            }
            let a = app::App::new(cc, cfg);
            #[cfg(feature = "selftest")]
            if tr {
                eprintln!("[trace] App::new listo: {:?}", T0.get().map(|t| t.elapsed()));
            }
            Ok(Box::new(a))
        }))
}

/// Punto de entrada de escritorio.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn run() -> eframe::Result {
    #[cfg(feature = "selftest")]
    let _ = T0.set(web_time::Instant::now());
    // `ssh` nos llama como programa de contraseñas (SSH_ASKPASS) para conectar por SFTP
    sftp::askpass_main();
    if let system::Cli::Exit(code) = system::start() {
        std::process::exit(code);
    }
    // espejos SFTP de ejecuciones anteriores (cerradas sin limpiar): fuera, en segundo plano
    std::thread::spawn(sftp::cleanup_stale);
    let cfg = config::Config::load();
    // antes del bucle de eventos: los archivos con los que macOS lanza la app llegan durante su arranque
    #[cfg(target_os = "macos")]
    system::install_open_files();
    let r = run_with(options(&cfg), cfg);
    // sin consola (subsistema gráfico) un fallo al abrir la ventana no se vería: el doble clic no haría nada
    #[cfg(windows)]
    if let Err(e) = &r {
        system::message_box(
            &i18n::tf(
                "File Browser no ha podido abrir su ventana: {}\n\nNecesita OpenGL 2.0 o posterior. Actualice el controlador de la tarjeta gráfica; en una máquina virtual o un escritorio remoto puede no estar disponible.",
                &[&e.to_string()],
            ),
            true,
        );
    }
    sftp::shutdown();
    system::shutdown();
    r
}

/// Punto de entrada de Android (NativeActivity).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    // El bucle de eventos de winit sólo puede crearse una vez por proceso: si la actividad se
    // recreara en el mismo proceso no podríamos volver a dibujar (y la actividad guardada en
    // ANDROID sería la vieja). Se sale y el siguiente arranque empieza en un proceso nuevo.
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::process::exit(0);
    }
    // en Android no hay HOME ni XDG: usamos el almacenamiento compartido y la carpeta de datos de la app
    if let Some(p) = app.internal_data_path() {
        // SAFETY: todavía no hay otros hilos nuestros que lean el entorno.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", p) };
    }
    // SAFETY: ídem.
    unsafe { std::env::set_var("HOME", "/storage/emulated/0") };
    let _ = ANDROID.set((app.vm_as_ptr() as usize, app.activity_as_ptr() as usize));
    immersive();
    allow_file_uris();
    // Android 8–10: el permiso de almacenamiento se pide en tiempo de ejecución
    if android_sdk() < 30 && !android_all_files() {
        android_request_all_files();
    }
    let cfg = config::Config::load();
    let mut opts = options(&cfg);
    opts.android_app = Some(app);
    let _ = run_with(opts, cfg);
    // la actividad ha terminado: proceso nuevo en el próximo arranque (ver arriba)
    std::process::exit(0);
}

/// Android no tiene papelera: el borrado a papelera devuelve error en lugar de borrar.
#[cfg(target_os = "android")]
pub mod trash {
    pub fn delete<P: AsRef<std::path::Path>>(_p: P) -> Result<(), String> {
        Err(crate::i18n::t("la papelera no está disponible en Android").into())
    }
}

#[cfg(target_os = "android")]
static ANDROID: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();

/// Ejecuta `f` con un JNIEnv de este hilo y la actividad, dentro de un marco local. Al acabar limpia
/// cualquier excepción Java pendiente: con una pendiente, la siguiente llamada JNI hace abortar el
/// proceso a ART. Dentro de `f`, tras una llamada cuyo error se ignore, hay que usar `jni_clear`.
#[cfg(target_os = "android")]
fn with_env<T>(f: impl FnOnce(&mut jni::JNIEnv, &jni::objects::JObject) -> jni::errors::Result<T>) -> Option<T> {
    let &(vm, act) = ANDROID.get()?;
    // SAFETY: `vm` es el JavaVM* que entrega android-activity y vale mientras viva el proceso.
    let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    // SAFETY: `act` es la referencia global a la actividad que mantiene android-activity (el proceso
    // termina con la actividad, ver `android_main`); un JObject no libera la referencia al soltarse.
    let act = unsafe { jni::objects::JObject::from_raw(act as jni::sys::jobject) };
    let r = env.with_local_frame(32, |env| f(env, &act));
    jni_clear(&mut env);
    r.ok()
}

#[cfg(target_os = "android")]
fn jni_clear(env: &mut jni::JNIEnv) {
    if env.exception_check().unwrap_or(true) {
        let _ = env.exception_clear();
    }
}

/// `Build.VERSION.SDK_INT` (0 si no se pudo leer).
#[cfg(target_os = "android")]
fn sdk_int(env: &mut jni::JNIEnv) -> i32 {
    match env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I").and_then(|v| v.i()) {
        Ok(v) => v,
        Err(_) => {
            jni_clear(env);
            0
        }
    }
}

#[cfg(target_os = "android")]
fn android_sdk() -> i32 {
    with_env(|env, _| Ok(sdk_int(env))).unwrap_or(0)
}

/// Pantalla completa inmersiva: oculta barra de estado y de navegación y usa la zona del notch.
#[cfg(target_os = "android")]
fn immersive() {
    use jni::objects::JValue;
    let r = with_env(|env, act| {
        let win = env.call_method(act, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
        let decor = env.call_method(&win, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
        // LAYOUT_STABLE | LAYOUT_HIDE_NAVIGATION | LAYOUT_FULLSCREEN | HIDE_NAVIGATION | FULLSCREEN | IMMERSIVE_STICKY
        env.call_method(&decor, "setSystemUiVisibility", "(I)V", &[JValue::Int(0x1706)])?;
        // contenido también bajo el recorte de cámara (notch): el campo existe desde Android 9 (API 28)
        if sdk_int(env) >= 28 {
            let attrs = env.call_method(&win, "getAttributes", "()Landroid/view/WindowManager$LayoutParams;", &[])?.l()?;
            if env.set_field(&attrs, "layoutInDisplayCutoutMode", "I", JValue::Int(1)).is_err() {
                jni_clear(env);
            }
            env.call_method(&win, "setAttributes", "(Landroid/view/WindowManager$LayoutParams;)V", &[JValue::Object(&attrs)])?;
        }
        Ok(())
    });
    if r.is_none() {
        eprintln!("aviso: no se pudo activar el modo inmersivo");
    }
}

/// Márgenes seguros del sistema en píxeles (izquierda, arriba, derecha, abajo): recorte de la
/// cámara, barras del sistema y esquinas. Cero fuera de Android o si aún no hay ventana.
#[cfg(target_os = "android")]
pub fn android_insets() -> [f32; 4] {
    // `with_env` abre un marco local: se consulta cada medio segundo y sin él las referencias JNI se
    // acumulan hasta abortar
    with_env(|env, act| {
        let win = env.call_method(act, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
        let decor = env.call_method(&win, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
        let wi = env.call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[])?.l()?;
        if wi.is_null() {
            return Ok([0.0; 4]);
        }
        // DisplayCutout sólo existe desde Android 9 (API 28)
        let dc = if sdk_int(env) >= 28 {
            env.call_method(&wi, "getDisplayCutout", "()Landroid/view/DisplayCutout;", &[])?.l()?
        } else {
            jni::objects::JObject::null()
        };
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
            if !dc.is_null() {
                b = env.call_method(&dc, cut, "()I", &[])?.i()?;
            }
            out[i] = a.max(b) as f32;
        }
        Ok(out)
    })
    .unwrap_or([0.0; 4])
}

#[cfg(not(target_os = "android"))]
pub fn android_insets() -> [f32; 4] {
    [0.0; 4]
}

/// ¿Tiene la app el acceso a todos los archivos? Sin él Android sólo deja ver carpetas y medios.
/// En Android 8–10 (API < 30) equivale al permiso de almacenamiento concedido en tiempo de ejecución.
#[cfg(target_os = "android")]
pub fn android_all_files() -> bool {
    use jni::objects::JValue;
    with_env(|env, act| {
        if sdk_int(env) < 30 {
            let perm = env.new_string("android.permission.WRITE_EXTERNAL_STORAGE")?;
            // checkSelfPermission (API 23): 0 = PERMISSION_GRANTED
            let r = env.call_method(act, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&perm)])?.i()?;
            return Ok(r == 0);
        }
        env.call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?.z()
    })
    .unwrap_or(true)
}

#[cfg(not(target_os = "android"))]
pub fn android_all_files() -> bool {
    true
}

/// Pide el acceso a los archivos: en Android 11+ abre la pantalla de ajustes del acceso a todos
/// los archivos; antes, el diálogo del permiso de almacenamiento. Si el diálogo ya no se muestra
/// («no volver a preguntar») o falta la pantalla de ajustes, abre la información de la app.
#[cfg(target_os = "android")]
pub fn android_request_all_files() {
    use jni::objects::JValue;
    use std::sync::atomic::{AtomicBool, Ordering};
    // ¿se pidió ya el permiso en este proceso? (al arrancar, en Android 8–10)
    static ASKED: AtomicBool = AtomicBool::new(false);
    /// Abre una pantalla de ajustes `action`, con `package:<app>` como dato si `with_pkg`.
    fn settings(env: &mut jni::JNIEnv, act: &jni::objects::JObject, action: &str, with_pkg: bool) -> jni::errors::Result<()> {
        let action = env.new_string(action)?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        if with_pkg {
            let pkg = env.call_method(act, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
            let pkg: String = env.get_string(&pkg.into())?.into();
            let uri_s = env.new_string(format!("package:{pkg}"))?;
            let uri = env
                .call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&uri_s)])?
                .l()?;
            env.call_method(&intent, "setData", "(Landroid/net/Uri;)Landroid/content/Intent;", &[JValue::Object(&uri)])?;
        }
        env.call_method(act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    }
    const DETAILS: &str = "android.settings.APPLICATION_DETAILS_SETTINGS";
    let _ = with_env(|env, act| {
        if sdk_int(env) < 30 {
            let w = env.new_string("android.permission.WRITE_EXTERNAL_STORAGE")?;
            // ya pedido y Android no mostraría el diálogo (sin explicación pendiente = «no volver a
            // preguntar»): sólo queda concederlo en la información de la app
            let again = ASKED.swap(true, Ordering::SeqCst);
            if again {
                let why = env
                    .call_method(act, "shouldShowRequestPermissionRationale", "(Ljava/lang/String;)Z", &[JValue::Object(&w)])
                    .and_then(|v| v.z());
                if !why.unwrap_or_else(|_| {
                    jni_clear(env);
                    false
                }) {
                    return settings(env, act, DETAILS, true);
                }
            }
            let r = env.new_string("android.permission.READ_EXTERNAL_STORAGE")?;
            let string_cls = env.find_class("java/lang/String")?;
            let arr = env.new_object_array(2, &string_cls, &w)?;
            env.set_object_array_element(&arr, 1, &r)?;
            // Activity.requestPermissions (API 23); la respuesta no hace falta: se vuelve a mirar
            // `android_all_files` al listar
            env.call_method(act, "requestPermissions", "([Ljava/lang/String;I)V", &[JValue::Object(&arr), JValue::Int(1)])?;
            return Ok(());
        }
        // algunos fabricantes no tienen la pantalla propia del permiso: la general y, si no, la de la app
        for (action, with_pkg) in [
            ("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION", true),
            ("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION", false),
            (DETAILS, true),
        ] {
            if settings(env, act, action, with_pkg).is_ok() {
                return Ok(());
            }
            jni_clear(env);
        }
        Ok(())
    });
}

#[cfg(not(target_os = "android"))]
pub fn android_request_all_files() {}

/// Desactiva la comprobación de `file://` (StrictMode) para poder entregar archivos a otras apps.
#[cfg(target_os = "android")]
fn allow_file_uris() {
    let r = with_env(|env, _| {
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
    });
    if r.is_none() {
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
    if ANDROID.get().is_none() {
        return Err(crate::i18n::t("sin actividad").into());
    }
    let mime = android_mime(path);
    let p = path.to_string_lossy().into_owned();
    let mut err = String::new();
    // `with_env` limpia la excepción pendiente si algo falla, para que la app no caiga
    let res = with_env(|env, act| {
        let r = (|| -> jni::errors::Result<()> {
        let jp = env.new_string(&p)?;
        let file = env.new_object("java/io/File", "(Ljava/lang/String;)V", &[JValue::Object(&jp)])?;
        let uri = env
            .call_static_method("android/net/Uri", "fromFile", "(Ljava/io/File;)Landroid/net/Uri;", &[JValue::Object(&file)])?
            .l()?;
        // Se entrega como content:// con el id del archivo en MediaStore: desde Android 11 otra app no
        // puede leer un file:// fuera de los medios (PDF, DOCX, ZIP…), y el instalador de paquetes sólo
        // acepta content://. Sólo si el archivo no está en MediaStore se cae a file://.
        let apk = mime == "application/vnd.android.package-archive";
        let mut uri = uri;
        let mut action_name = "android.intent.action.VIEW";
        // si la consulta falla (excepción), se sigue con file://
        let media = (|| -> jni::errors::Result<Option<JObject>> {
            let resolver = env
                .call_method(act, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?
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
            let mut found = None;
            if !cur.is_null() {
                if env.call_method(&cur, "moveToFirst", "()Z", &[])?.z()? {
                    let id = env.call_method(&cur, "getLong", "(I)J", &[JValue::Int(0)])?.j()?;
                    found = Some(env
                        .call_static_method(
                            "android/content/ContentUris",
                            "withAppendedId",
                            "(Landroid/net/Uri;J)Landroid/net/Uri;",
                            &[JValue::Object(&base), JValue::Long(id)],
                        )?
                        .l()?);
                }
                env.call_method(&cur, "close", "()V", &[])?;
            }
            Ok(found)
        })();
        match media {
            Ok(Some(u)) => {
                uri = u;
                if apk {
                    action_name = "android.intent.action.INSTALL_PACKAGE";
                }
            }
            Ok(None) => {}
            Err(_) => jni_clear(env),
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
            env.call_method(act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
            return Ok(());
        }
        let title = env.new_string(crate::i18n::t("Abrir con"))?;
        let chooser = env
            .call_static_method(
                "android/content/Intent",
                "createChooser",
                "(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;",
                &[JValue::Object(&intent), JValue::Object(&title)],
            )?
            .l()?;
        env.call_method(act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&chooser)])?;
        Ok(())
        })();
        if let Err(e) = &r {
            err = e.to_string();
        }
        r
    });
    match res {
        Some(()) => Ok(()),
        None if err.is_empty() => Err(crate::i18n::t("sin acceso a Java").into()),
        None => Err(err),
    }
}

/// Arranque en el navegador: la misma aplicación dibujada en el `<canvas>` de la página que sirve
/// `filebrowser-server`. `writable` dice si el servidor deja modificar archivos.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start(canvas: web_sys::HtmlCanvasElement, writable: bool) -> Result<(), wasm_bindgen::JsValue> {
    std::panic::set_hook(Box::new(|info| web_sys::console::error_1(&format!("File Browser: {info}").into())));
    vfs::net::set_writable(writable);
    let cfg = config::Config::load();
    eframe::WebRunner::new()
        .start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg)))))
        .await
}
