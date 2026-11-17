//! Poner la ventana delante en Wayland con el token de activación de otra aplicación
//! (xdg-activation-v1). winit 0.30 sólo usa un token al crear la ventana y su `focus_window` no
//! hace nada en Wayland, así que se habla con el compositor por nuestra cuenta sobre la misma
//! conexión de winit (cola de eventos propia).
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{delegate_noop, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

struct St;

impl Dispatch<WlRegistry, GlobalListContents> for St {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(St: ignore XdgActivationV1);

/// Pide al compositor que active la ventana con `token`. `false` si no es Wayland, si el
/// compositor no tiene xdg-activation o si algo falla (el compositor aún puede negarse).
pub fn activate(frame: &eframe::Frame, token: &str) -> bool {
    let (Ok(d), Ok(w)) = (frame.display_handle(), frame.window_handle()) else { return false };
    let (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) = (d.as_raw(), w.as_raw()) else { return false };
    // SAFETY: es el wl_display de winit, vivo mientras la ventana (y más que esta función).
    let backend = unsafe { Backend::from_foreign_display(d.display.as_ptr().cast()) };
    let conn = Connection::from_backend(backend);
    let Ok((globals, queue)) = registry_queue_init::<St>(&conn) else { return false };
    let qh = queue.handle();
    let Ok(act) = globals.bind::<XdgActivationV1, _, _>(&qh, 1..=1, ()) else { return false };
    // SAFETY: es el wl_surface de la ventana de winit, vivo mientras dure el Frame.
    let surface = unsafe { ObjectId::from_ptr(WlSurface::interface(), w.surface.as_ptr().cast()) }
        .ok()
        .and_then(|id| WlSurface::from_id(&conn, id).ok());
    if let Some(surface) = &surface {
        act.activate(token.to_owned(), surface);
    }
    act.destroy();
    surface.is_some() && conn.flush().is_ok()
}
