//! Las peticiones a `filebrowser-server`. La página vive en `/s/<token>/app/`, así que las direcciones
//! relativas (`../api/...`) quedan dentro del token.
//!
//! Casi todas son síncronas (el programa espera la respuesta, como con un disco). Una petición síncrona
//! no admite plazo (`timeout`) en la página principal: el navegador lo prohíbe (InvalidAccessError); el
//! servidor corta las suyas. Las subidas de archivos elegidos o soltados van aparte, asíncronas y desde
//! el propio archivo del navegador (sin copiarlo a memoria), con progreso y botón de cancelar.
use crate::i18n::{t, tf};
use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::XmlHttpRequest;

thread_local! {
    static WRITABLE: Cell<bool> = const { Cell::new(false) };
    /// Tope de subida del servidor (`/api/info`), preguntado la primera vez; `u64::MAX` si no lo dice.
    static MAX_UPLOAD: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Si el servidor deja modificar archivos (`--write`); se sabe al arrancar.
pub fn writable() -> bool {
    WRITABLE.get()
}

pub fn set_writable(w: bool) {
    WRITABLE.set(w);
}

pub fn now() -> f64 {
    js_sys::Date::now() / 1000.0
}

fn enc(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}

/// Ruta virtual codificada por partes, para ponerla en la dirección.
fn url_path(p: &str) -> String {
    p.split('/').filter(|c| !c.is_empty()).map(enc).collect::<Vec<_>>().join("/")
}

fn net_err() -> io::Error {
    io::Error::other(t("no se pudo conectar con el servidor"))
}

fn read_only() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, t("el servidor es de solo lectura"))
}

fn xhr(method: &str, url: &str) -> io::Result<XmlHttpRequest> {
    let x = XmlHttpRequest::new().map_err(|_| net_err())?;
    x.open_with_async(method, url, false).map_err(|_| net_err())?;
    // los bytes llegan como texto con un solo carácter por byte (no se puede pedir un búfer en una
    // petición síncrona de la página principal)
    x.override_mime_type("text/plain; charset=x-user-defined").map_err(|_| net_err())?;
    // el servidor exige esta cabecera a las peticiones que modifican (una página ajena no puede ponerla)
    x.set_request_header("X-FB", "1").map_err(|_| net_err())?;
    Ok(x)
}

/// Tope de subida del servidor en bytes (`u64::MAX` si un servidor antiguo no lo publica).
pub fn max_upload() -> u64 {
    if let Some(m) = MAX_UPLOAD.get() {
        return m;
    }
    let Ok(x) = xhr("GET", "../api/info") else { return u64::MAX };
    let Ok(b) = finish(&x, None) else { return u64::MAX };
    let m = serde_json::from_slice::<serde_json::Value>(&b).ok().and_then(|v| v.get("max_upload").and_then(|m| m.as_u64())).unwrap_or(u64::MAX);
    MAX_UPLOAD.set(Some(m));
    m
}

fn too_large(max: u64) -> io::Error {
    let msg = if max == u64::MAX { t("demasiado grande para el servidor").to_string() } else { tf("demasiado grande para el servidor (máximo {} MiB)", &[&(max >> 20).max(1)]) };
    io::Error::new(io::ErrorKind::FileTooLarge, msg)
}

/// Error para un código HTTP y el motivo JSON que manda el servidor en las escrituras (`write.rs`).
fn status_err(code: u16, body: &[u8]) -> io::Error {
    use io::ErrorKind as K;
    let why = serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string)).unwrap_or_default();
    let (kind, msg) = match (code, why.as_str()) {
        (_, "hidden") => (K::PermissionDenied, t("contiene archivos ocultos, que no se pueden borrar ni mover desde la web")),
        // ni `Other` (sería «otro sistema de archivos» y `fsys` copiaría el árbol entero)
        (_, "too_big") => (K::FileTooLarge, t("la carpeta es demasiado grande o profunda para hacerlo desde la web")),
        (_, "unreadable") => (K::PermissionDenied, t("hay una subcarpeta que no se puede leer")),
        (_, "not_empty") => (K::DirectoryNotEmpty, t("la carpeta no está vacía")),
        (405, _) | (_, "read_only") => (K::PermissionDenied, t("el servidor es de solo lectura")),
        (413, _) => return too_large(MAX_UPLOAD.get().unwrap_or(u64::MAX)),
        (409 | 412, _) => (K::AlreadyExists, t("ya existe")),
        (401 | 403, _) => (K::PermissionDenied, t("sin permiso")),
        (404, _) => (K::NotFound, t("no existe")),
        (429, _) => (K::ResourceBusy, t("demasiados intentos: espera un minuto y vuelve a probar")),
        (507, _) => (K::StorageFull, t("no queda espacio en el servidor")),
        (0, _) => return net_err(),
        // 400 (p. ej. entre carpetas compartidas distintas) queda como `Other`: `fsys` lo trata como
        // «otro sistema de archivos» y copia
        _ => return io::Error::other(tf("el servidor respondió {}", &[&code])),
    };
    io::Error::new(kind, msg)
}

fn bytes_of(x: &XmlHttpRequest) -> Vec<u8> {
    x.response_text().ok().flatten().unwrap_or_default().encode_utf16().map(|u| (u & 0xff) as u8).collect()
}

fn finish(x: &XmlHttpRequest, body: Option<&[u8]>) -> io::Result<Vec<u8>> {
    let sent = match body {
        Some(b) => {
            let arr = js_sys::Uint8Array::from(b);
            x.send_with_opt_buffer_source(Some(&arr))
        }
        None => x.send(),
    };
    sent.map_err(|_| net_err())?;
    let code = x.status().unwrap_or(0);
    if !(200..300).contains(&code) {
        return Err(status_err(code, &bytes_of(x)));
    }
    Ok(bytes_of(x))
}

thread_local! {
    /// Carpetas recortadas de las que ya se avisó (una vez por sesión).
    static TRUNC_SEEN: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Entradas de una carpeta: (nombre, es carpeta, tamaño, fecha). Si el servidor la recorta (demasiadas
/// entradas) se avisa una vez.
pub fn list(path: &str) -> io::Result<Vec<(String, bool, u64, i64)>> {
    let x = xhr("GET", &format!("../api/list?p={}", enc(path)))?;
    let body = finish(&x, None)?;
    let text = String::from_utf8_lossy(&body);
    // el cuerpo es JSON en UTF-8 leído byte a byte
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| io::Error::other(crate::i18n::tf("respuesta no válida: {}", &[&text.chars().take(60).collect::<String>()])))?;
    let mut out = Vec::new();
    for e in v.get("entries").and_then(|e| e.as_array()).into_iter().flatten() {
        let Some(n) = e.get("n").and_then(|n| n.as_str()) else { continue };
        out.push((n.to_string(), e.get("d").and_then(|d| d.as_bool()).unwrap_or(false), e.get("s").and_then(|s| s.as_u64()).unwrap_or(0), e.get("m").and_then(|m| m.as_i64()).unwrap_or(0)));
    }
    if v.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false) && TRUNC_SEEN.with_borrow_mut(|s| s.insert(path.to_string())) {
        notice(&tf("Esta carpeta tiene demasiadas entradas: solo se muestran las primeras {}.", &[&out.len()]));
    }
    Ok(out)
}

/// `len` bytes del archivo desde `start`.
pub fn range(path: &str, start: u64, len: u64) -> io::Result<Vec<u8>> {
    let x = xhr("GET", &format!("../{}", url_path(path)))?;
    x.set_request_header("Range", &format!("bytes={}-{}", start, start + len.max(1) - 1)).map_err(|_| net_err())?;
    finish(&x, None)
}

/// Guarda un archivo entero. `only_new`: solo si no existe (`If-None-Match: *`; si existe, `AlreadyExists`).
pub fn put(path: &str, data: &[u8], only_new: bool) -> io::Result<()> {
    if !writable() {
        return Err(read_only());
    }
    let max = max_upload();
    if data.len() as u64 > max {
        return Err(too_large(max));
    }
    let x = xhr("PUT", &format!("../{}", url_path(path)))?;
    if only_new {
        x.set_request_header("If-None-Match", "*").map_err(|_| net_err())?;
    }
    finish(&x, Some(data)).map(|_| ())
}

pub fn op(name: &str, from: &str, to: Option<&str>) -> io::Result<()> {
    if !writable() {
        return Err(read_only());
    }
    let mut url = format!("../api/op?op={}&p={}", name, enc(from));
    if let Some(t) = to {
        url.push_str(&format!("&to={}", enc(t)));
    }
    let x = xhr("POST", &url)?;
    finish(&x, None).map(|_| ())
}

// ---------- subidas ----------

/// Un archivo del navegador por subir y la carpeta a la que va (la abierta cuando se eligió).
struct Job {
    dir: PathBuf,
    file: web_sys::File,
}

thread_local! {
    /// Elegidos con «Subir…» o soltados en la página, aún sin carpeta asignada.
    static PICKED: RefCell<Vec<web_sys::File>> = const { RefCell::new(Vec::new()) };
    static JOBS: RefCell<VecDeque<Job>> = const { RefCell::new(VecDeque::new()) };
    /// Terminadas desde el último fotograma: (nombre, resultado).
    static RESULTS: RefCell<Vec<(String, Result<(), String>)>> = const { RefCell::new(Vec::new()) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static CANCEL: Cell<bool> = const { Cell::new(false) };
    static CURRENT: RefCell<Option<XmlHttpRequest>> = const { RefCell::new(None) };
    static CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    /// Barra de progreso (caja y texto), creada en la página la primera vez.
    static BAR: RefCell<Option<(web_sys::Element, web_sys::Element)>> = const { RefCell::new(None) };
}

fn repaint() {
    CTX.with_borrow(|c| {
        if let Some(c) = c {
            c.request_repaint();
        }
    });
}

fn add_picked(f: web_sys::File, ctx: &egui::Context) {
    CTX.with_borrow_mut(|c| *c = Some(ctx.clone()));
    PICKED.with_borrow_mut(|p| p.push(f));
    ctx.request_repaint();
}

/// Cada fotograma: lo elegido desde el anterior se encola para subirlo a `dir` (la carpeta abierta) y
/// se devuelven las subidas terminadas: (nombre, resultado).
pub fn run_uploads(dir: &Path) -> Vec<(String, Result<(), String>)> {
    let picked = PICKED.with_borrow_mut(std::mem::take);
    if !picked.is_empty() {
        JOBS.with_borrow_mut(|j| j.extend(picked.into_iter().map(|file| Job { dir: dir.to_path_buf(), file })));
        if !BUSY.get() {
            BUSY.set(true);
            CANCEL.set(false);
            wasm_bindgen_futures::spawn_local(worker());
        }
    }
    RESULTS.with_borrow_mut(std::mem::take)
}

async fn worker() {
    while let Some(job) = JOBS.with_borrow_mut(|j| j.pop_front()) {
        let name = job.file.name();
        let r = upload_one(&job, &name).await.map_err(|e| e.to_string());
        RESULTS.with_borrow_mut(|v| v.push((name, r)));
        repaint();
    }
    BUSY.set(false);
    bar_hide();
    repaint();
}

/// Sube un archivo con un nombre libre en su carpeta: si otro aparece con ese nombre entretanto, el
/// servidor no lo pisa (`If-None-Match: *`) y se prueba el siguiente.
async fn upload_one(job: &Job, name: &str) -> io::Result<()> {
    if !writable() {
        return Err(read_only());
    }
    // antes de enviar nada: el tope del servidor
    let max = max_upload();
    if job.file.size() > max as f64 {
        return Err(too_large(max));
    }
    for _ in 0..8 {
        crate::vfs::fs::invalidate(&job.dir);
        let dst = crate::fsys::unique_path(&job.dir, name);
        let r = send_blob(&crate::vfs::fs::virt(&dst), &job.file, name).await;
        crate::vfs::fs::invalidate(&job.dir);
        match r {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            r => return r,
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, t("ya existe")))
}

async fn send_blob(path: &str, file: &web_sys::File, name: &str) -> io::Result<()> {
    let x = XmlHttpRequest::new().map_err(|_| net_err())?;
    x.open_with_async("PUT", &format!("../{}", url_path(path)), true).map_err(|_| net_err())?;
    x.set_request_header("X-FB", "1").map_err(|_| net_err())?;
    x.set_request_header("If-None-Match", "*").map_err(|_| net_err())?;
    let total = file.size();
    let label = name.to_string();
    bar_show(&tf("Subiendo {}: {} %", &[&label, &0]));
    let on_prog = Closure::<dyn FnMut(web_sys::ProgressEvent)>::new(move |e: web_sys::ProgressEvent| {
        let tot = if e.length_computable() { e.total() } else { total };
        let pct = if tot > 0.0 { ((e.loaded() / tot) * 100.0).clamp(0.0, 100.0) as u32 } else { 0 };
        bar_show(&tf("Subiendo {}: {} %", &[&label, &pct]));
    });
    if let Ok(up) = x.upload() {
        up.set_onprogress(Some(on_prog.as_ref().unchecked_ref()));
    }
    let xc = x.clone();
    let done = js_sys::Promise::new(&mut |resolve, _| xc.set_onloadend(Some(&resolve)));
    CURRENT.with_borrow_mut(|c| *c = Some(x.clone()));
    let sent = x.send_with_opt_blob(Some(file));
    if sent.is_ok() {
        let _ = wasm_bindgen_futures::JsFuture::from(done).await;
    }
    CURRENT.with_borrow_mut(|c| *c = None);
    if let Ok(up) = x.upload() {
        up.set_onprogress(None);
    }
    x.set_onloadend(None);
    drop(on_prog);
    sent.map_err(|_| net_err())?;
    if CANCEL.get() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, t("subida cancelada")));
    }
    let code = x.status().unwrap_or(0);
    if (200..300).contains(&code) {
        return Ok(());
    }
    let body = x.response_text().ok().flatten().unwrap_or_default();
    Err(status_err(code, body.as_bytes()))
}

/// «Cancelar»: corta la subida en curso y descarta las que esperaban.
fn cancel_uploads() {
    CANCEL.set(true);
    JOBS.with_borrow_mut(|j| j.clear());
    if let Some(x) = CURRENT.with_borrow(|c| c.clone()) {
        let _ = x.abort();
    }
}

const BOX_STYLE: &str = "position:fixed;left:50%;transform:translateX(-50%);z-index:10;display:flex;gap:12px;align-items:center;\
    max-width:90vw;box-sizing:border-box;background:#2b2d33;color:#e6e6e9;font:14px system-ui,sans-serif;padding:8px 12px;\
    border-radius:6px;box-shadow:0 2px 8px #0008";

fn make_bar() -> Option<(web_sys::Element, web_sys::Element)> {
    let doc = web_sys::window()?.document()?;
    let div = doc.create_element("div").ok()?;
    let span = doc.create_element("span").ok()?;
    let btn = doc.create_element("button").ok()?;
    let _ = div.set_attribute("role", "status");
    let _ = span.set_attribute("style", "overflow:hidden;text-overflow:ellipsis;white-space:nowrap");
    btn.set_text_content(Some(t("Cancelar")));
    let cb = Closure::<dyn FnMut()>::new(cancel_uploads);
    let _ = btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
    // la barra vive lo que la página
    cb.forget();
    div.append_child(&span).ok()?;
    div.append_child(&btn).ok()?;
    doc.body()?.append_child(&div).ok()?;
    Some((div, span))
}

fn bar_show(text: &str) {
    BAR.with_borrow_mut(|b| {
        if b.is_none() {
            *b = make_bar();
        }
        if let Some((div, span)) = b {
            span.set_text_content(Some(text));
            let _ = div.set_attribute("style", &format!("{BOX_STYLE};bottom:48px"));
        }
    });
}

fn bar_hide() {
    BAR.with_borrow(|b| {
        if let Some((div, _)) = b {
            let _ = div.set_attribute("style", "display:none");
        }
    });
}

/// Aviso breve sobre la página (unos segundos), para lo que no pasa por un diálogo de la aplicación.
pub fn notice(text: &str) {
    let Some(w) = web_sys::window() else { return };
    let Some(doc) = w.document() else { return };
    let Ok(div) = doc.create_element("div") else { return };
    let _ = div.set_attribute("role", "status");
    let _ = div.set_attribute("style", &format!("{BOX_STYLE};top:48px"));
    div.set_text_content(Some(text));
    let Some(body) = doc.body() else { return };
    if body.append_child(&div).is_err() {
        return;
    }
    let gone = Closure::once_into_js(move || div.remove());
    let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(gone.unchecked_ref(), 8000);
}

/// Un archivo soltado en la página: se sube desde el propio archivo del navegador.
pub fn read_dropped(f: std::sync::Arc<dyn egui::DroppedFile>, ctx: egui::Context) {
    if let Some(file) = f.web_file() {
        add_picked(file.clone(), &ctx);
        return;
    }
    // sin el archivo del navegador: se lee entero y se sube desde memoria
    wasm_bindgen_futures::spawn_local(async move {
        let name = f.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Ok(b) = f.bytes_async().await else { return };
        if name.is_empty() {
            return;
        }
        let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(&b[..]));
        if let Ok(file) = web_sys::File::new_with_u8_array_sequence(&parts, &name) {
            add_picked(file, &ctx);
        }
    });
}

/// Abre el selector de archivos del navegador; lo elegido se sube con `run_uploads`.
pub fn pick_files(ctx: egui::Context) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(el) = doc.create_element("input") else { return };
    let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() else { return };
    input.set_type("file");
    input.set_multiple(true);
    let inp = input.clone();
    let on_change = Closure::once_into_js(move || {
        let Some(files) = inp.files() else { return };
        for i in 0..files.length() {
            if let Some(file) = files.get(i) {
                add_picked(file, &ctx);
            }
        }
        inp.remove();
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    // algunos navegadores solo abren el selector si el campo está en el documento
    let _ = input.set_attribute("style", "display:none");
    if let Some(body) = doc.body() {
        let _ = body.append_child(&input);
    }
    input.click();
}

/// Abre un archivo del servidor en una pestaña nueva (el navegador lo muestra o lo descarga).
pub fn open_path(path: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target_and_features(&format!("../{}", url_path(path)), "_blank", "noopener");
    }
}

/// Almacén del navegador (`localStorage`): la configuración se guarda aquí, no en el servidor.
pub fn store_get(key: &str) -> Option<String> {
    web_sys::window()?.local_storage().ok()??.get_item(key).ok()?
}

pub fn store_set(key: &str, val: &str) {
    if let Some(Ok(Some(st))) = web_sys::window().map(|w| w.local_storage()) {
        let _ = st.set_item(key, val);
    }
}
