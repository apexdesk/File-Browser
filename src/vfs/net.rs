//! Las peticiones a `filebrowser-server`, síncronas. La página vive en `/s/<token>/app/`, así que
//! las direcciones relativas (`../api/...`) quedan dentro del token.
use std::cell::{Cell, RefCell};
use std::io;
use web_sys::XmlHttpRequest;

thread_local! {
    static WRITABLE: Cell<bool> = const { Cell::new(false) };
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

fn xhr(method: &str, url: &str) -> io::Result<XmlHttpRequest> {
    let x = XmlHttpRequest::new().map_err(|_| io::Error::other("sin red"))?;
    x.open_with_async(method, url, false).map_err(|_| io::Error::other("dirección no válida"))?;
    // los bytes llegan como texto con un solo carácter por byte (no se puede pedir un búfer en una
    // petición síncrona de la página principal)
    x.override_mime_type("text/plain; charset=x-user-defined").map_err(|_| io::Error::other("sin red"))?;
    // el servidor exige esta cabecera a las peticiones que modifican (una página ajena no puede ponerla)
    x.set_request_header("X-FB", "1").map_err(|_| io::Error::other("sin red"))?;
    Ok(x)
}

fn status_err(code: u16) -> io::Error {
    match code {
        401 | 403 => io::Error::new(io::ErrorKind::PermissionDenied, "sin permiso"),
        404 => io::Error::new(io::ErrorKind::NotFound, "no existe"),
        409 => io::Error::new(io::ErrorKind::AlreadyExists, "ya existe"),
        _ => io::Error::other(format!("el servidor respondió {code}")),
    }
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
    sent.map_err(|_| io::Error::other("no se pudo conectar con el servidor"))?;
    let code = x.status().unwrap_or(0);
    if !(200..300).contains(&code) {
        return Err(status_err(code));
    }
    Ok(bytes_of(x))
}

/// Entradas de una carpeta: (nombre, es carpeta, tamaño, fecha).
pub fn list(path: &str) -> io::Result<Vec<(String, bool, u64, i64)>> {
    let x = xhr("GET", &format!("../api/list?p={}", enc(path)))?;
    let body = finish(&x, None)?;
    let text = String::from_utf8_lossy(&body);
    // el cuerpo es JSON en UTF-8 leído byte a byte
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| io::Error::other(format!("respuesta no válida: {}", text.chars().take(60).collect::<String>())))?;
    let mut out = Vec::new();
    for e in v.get("entries").and_then(|e| e.as_array()).into_iter().flatten() {
        let Some(n) = e.get("n").and_then(|n| n.as_str()) else { continue };
        out.push((n.to_string(), e.get("d").and_then(|d| d.as_bool()).unwrap_or(false), e.get("s").and_then(|s| s.as_u64()).unwrap_or(0), e.get("m").and_then(|m| m.as_i64()).unwrap_or(0)));
    }
    Ok(out)
}

/// `len` bytes del archivo desde `start`.
pub fn range(path: &str, start: u64, len: u64) -> io::Result<Vec<u8>> {
    let x = xhr("GET", &format!("../{}", url_path(path)))?;
    x.set_request_header("Range", &format!("bytes={}-{}", start, start + len.max(1) - 1)).map_err(|_| io::Error::other("sin red"))?;
    finish(&x, None)
}

pub fn put(path: &str, data: &[u8]) -> io::Result<()> {
    if !writable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "el servidor es de solo lectura"));
    }
    let x = xhr("PUT", &format!("../{}", url_path(path)))?;
    finish(&x, Some(data)).map(|_| ())
}

pub fn op(name: &str, from: &str, to: Option<&str>) -> io::Result<()> {
    if !writable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "el servidor es de solo lectura"));
    }
    let mut url = format!("../api/op?op={}&p={}", name, enc(from));
    if let Some(t) = to {
        url.push_str(&format!("&to={}", enc(t)));
    }
    let x = xhr("POST", &url)?;
    finish(&x, None).map(|_| ())
}

thread_local! {
    /// Archivos que el usuario ha soltado en la página o elegido con «Subir…», ya leídos.
    static UPLOADS: RefCell<Vec<(String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
}

/// Los archivos recibidos desde el último fotograma: (nombre, contenido).
pub fn take_uploads() -> Vec<(String, Vec<u8>)> {
    UPLOADS.with_borrow_mut(std::mem::take)
}

/// Lee un archivo soltado en la página (el navegador solo deja leerlo de forma asíncrona).
pub fn read_dropped(f: std::sync::Arc<dyn egui::DroppedFile>, ctx: egui::Context) {
    wasm_bindgen_futures::spawn_local(async move {
        let name = f.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match f.bytes_async().await {
            Ok(b) if !name.is_empty() => {
                UPLOADS.with_borrow_mut(|u| u.push((name, b)));
                ctx.request_repaint();
            }
            _ => {}
        }
    });
}

/// Abre el selector de archivos del navegador; lo elegido llega por `take_uploads`.
pub fn pick_files(ctx: egui::Context) {
    use wasm_bindgen::JsCast;
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(el) = doc.create_element("input") else { return };
    let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() else { return };
    input.set_type("file");
    input.set_multiple(true);
    let inp = input.clone();
    let on_change = wasm_bindgen::closure::Closure::once_into_js(move || {
        let Some(files) = inp.files() else { return };
        for i in 0..files.length() {
            let Some(file) = files.get(i) else { continue };
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let name = file.name();
                if let Ok(buf) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                    UPLOADS.with_borrow_mut(|u| u.push((name, js_sys::Uint8Array::new(&buf).to_vec())));
                    ctx.request_repaint();
                }
            });
        }
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    input.click();
}

/// Abre un archivo del servidor en una pestaña nueva (el navegador lo muestra o lo descarga).
pub fn open_path(path: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target(&format!("../{}", url_path(path)), "_blank");
    }
}
