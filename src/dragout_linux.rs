//! Arrastre hacia fuera en Linux: Wayland (wl_data_device) y X11 (XDND).
use crate::dragout::{parse_uri_list, uri_list};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::path::PathBuf;

pub enum Imp {
    Wl(Box<wl::Wl>),
    X11(x::X11),
}

impl Imp {
    pub fn new(frame: &eframe::Frame, ctx: &egui::Context) -> Option<Imp> {
        let ctx = ctx.clone();
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || ctx.request_repaint());
        let d = frame.display_handle().ok()?.as_raw();
        let w = frame.window_handle().ok()?.as_raw();
        match (d, w) {
            (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) => {
                wl::Wl::new(d.display.as_ptr() as usize, w.surface.as_ptr() as usize, wake).map(|v| Imp::Wl(Box::new(v)))
            }
            (RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_), RawWindowHandle::Xlib(w)) => {
                x::X11::new(w.window as u32).map(Imp::X11)
            }
            (RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_), RawWindowHandle::Xcb(w)) => {
                x::X11::new(w.window.get()).map(Imp::X11)
            }
            _ => None,
        }
    }

    pub fn pump(&mut self) {
        if let Imp::Wl(w) = self {
            w.pump();
        }
    }

    pub fn start(&mut self, paths: &[PathBuf]) -> bool {
        match self {
            Imp::Wl(w) => w.start(paths),
            Imp::X11(x) => x.start(paths),
        }
    }

    pub fn active(&self) -> bool {
        match self {
            Imp::Wl(w) => w.active(),
            Imp::X11(x) => x.active(),
        }
    }

    pub fn take_done(&mut self) -> bool {
        match self {
            Imp::Wl(w) => std::mem::take(&mut w.done),
            Imp::X11(x) => x.take_done(),
        }
    }

    pub fn take_dropped(&mut self) -> Vec<PathBuf> {
        match self {
            Imp::Wl(w) => w.take_dropped(),
            Imp::X11(_) => Vec::new(),
        }
    }
}

// ===========================================================================
// Wayland
// ===========================================================================
mod wl {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};
    use std::sync::{Arc, Mutex};
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_buffer::WlBuffer;
    use wayland_client::protocol::wl_compositor::WlCompositor;
    use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
    use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
    use wayland_client::protocol::wl_data_offer::WlDataOffer;
    use wayland_client::protocol::wl_data_source::{self, WlDataSource};
    use wayland_client::protocol::wl_pointer::{self, ButtonState, WlPointer};
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
    use wayland_client::protocol::wl_shm::{self, WlShm};
    use wayland_client::protocol::wl_shm_pool::WlShmPool;
    use wayland_client::protocol::wl_surface::WlSurface;
    use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};

    const URI: &str = "text/uri-list";
    const ICON: i32 = 56;

    struct St {
        /// Número de serie y estado del último botón del puntero.
        serial: u32,
        pressed: bool,
        has_pointer: bool,
        payload: Arc<Vec<u8>>,
        source_active: bool,
        ended: bool,
        surface_id: Option<ObjectId>,
        // recepción
        dropped: Arc<Mutex<Vec<PathBuf>>>,
        offer_has_uri: bool,
        over_ours: bool,
        last_enter: u32,
        cur_offer: Option<WlDataOffer>,
        wake: Arc<dyn Fn() + Send + Sync>,
    }

    pub struct Wl {
        conn: Connection,
        queue: EventQueue<St>,
        qh: QueueHandle<St>,
        st: St,
        origin: WlSurface,
        seat: WlSeat,
        manager: Option<WlDataDeviceManager>,
        compositor: Option<WlCompositor>,
        shm: Option<WlShm>,
        device: Option<WlDataDevice>,
        _pointer: Option<WlPointer>,
        source: Option<WlDataSource>,
        icon: Option<(WlSurface, WlBuffer)>,
        pub done: bool,
    }

    impl Dispatch<WlRegistry, GlobalListContents> for St {
        fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<WlSeat, ()> for St {
        fn event(st: &mut Self, _: &WlSeat, ev: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = ev {
                st.has_pointer = c.contains(Capability::Pointer);
            }
        }
    }

    impl Dispatch<WlPointer, ()> for St {
        fn event(st: &mut Self, _: &WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let wl_pointer::Event::Button { serial, state, .. } = ev {
                if state == WEnum::Value(ButtonState::Pressed) {
                    st.serial = serial;
                    st.pressed = true;
                } else {
                    st.pressed = false;
                }
            }
        }
    }

    impl Dispatch<WlDataDevice, ()> for St {
        fn event(st: &mut Self, _: &WlDataDevice, ev: wl_data_device::Event, _: &(), conn: &Connection, _: &QueueHandle<Self>) {
            match ev {
                wl_data_device::Event::Enter { serial, surface, id: offer, .. } => {
                    st.last_enter = serial;
                    st.over_ours = st.surface_id.as_ref().map(|id| *id == surface.id()).unwrap_or(false) && !st.source_active;
                    st.cur_offer = offer.clone();
                    if let Some(o) = offer {
                        if st.over_ours && st.offer_has_uri {
                            o.accept(serial, Some(URI.into()));
                            o.set_actions(DndAction::Copy, DndAction::Copy);
                        } else {
                            o.accept(serial, None);
                        }
                    }
                }
                wl_data_device::Event::DataOffer { .. } => st.offer_has_uri = false,
                wl_data_device::Event::Leave => st.over_ours = false,
                wl_data_device::Event::Drop => {
                    if let (true, Some(offer)) = (st.over_ours, st.cur_offer.take()) {
                        let mut fds = [0i32; 2];
                        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == 0 {
                            let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
                            offer.receive(URI.into(), wr.as_fd());
                            drop(wr);
                            let _ = conn.flush();
                            let (out, wake, conn) = (st.dropped.clone(), st.wake.clone(), conn.clone());
                            std::thread::spawn(move || {
                                let mut buf = Vec::new();
                                let _ = std::fs::File::from(rd).read_to_end(&mut buf);
                                out.lock().unwrap().extend(parse_uri_list(&buf));
                                offer.finish();
                                offer.destroy();
                                let _ = conn.flush();
                                wake();
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        wayland_client::event_created_child!(St, WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
        ]);
    }

    impl Dispatch<WlDataOffer, ()> for St {
        fn event(st: &mut Self, offer: &WlDataOffer, ev: wayland_client::protocol::wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let wayland_client::protocol::wl_data_offer::Event::Offer { mime_type } = ev {
                // el primer offer de cada objeto reinicia la marca
                let _ = offer;
                if mime_type == URI {
                    st.offer_has_uri = true;
                }
            }
        }
    }

    impl Dispatch<WlDataSource, ()> for St {
        fn event(st: &mut Self, _: &WlDataSource, ev: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match ev {
                wl_data_source::Event::Send { mime_type, fd } => {
                    if mime_type == URI || mime_type.starts_with("text/plain") {
                        let data = st.payload.clone();
                        std::thread::spawn(move || {
                            let mut f = std::fs::File::from(fd);
                            let _ = f.write_all(&data);
                        });
                    }
                }
                wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => st.ended = true,
                _ => {}
            }
        }
    }

    delegate_noop!(St: WlDataDeviceManager);
    delegate_noop!(St: WlCompositor);
    delegate_noop!(St: ignore WlShm);
    delegate_noop!(St: ignore WlShmPool);
    delegate_noop!(St: ignore WlBuffer);
    delegate_noop!(St: ignore WlSurface);

    fn icon_pixels() -> Vec<u8> {
        // icono de la aplicación reducido, en ARGB8888 premultiplicado (little endian: B,G,R,A)
        let img = image::load_from_memory(include_bytes!("../assets/icon-256.png")).map(|i| i.into_rgba8());
        let mut out = vec![0u8; (ICON * ICON * 4) as usize];
        if let Ok(img) = img {
            let r = image::imageops::resize(&img, ICON as u32, ICON as u32, image::imageops::FilterType::Triangle);
            for (i, p) in r.pixels().enumerate() {
                let a = p[3] as u32;
                let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                out[i * 4..i * 4 + 4].copy_from_slice(&[m(p[2]), m(p[1]), m(p[0]), p[3]]);
            }
        }
        out
    }

    impl Wl {
        pub fn new(display: usize, surface: usize, wake: Arc<dyn Fn() + Send + Sync>) -> Option<Wl> {
            let backend = unsafe { Backend::from_foreign_display(display as *mut _) };
            let conn = Connection::from_backend(backend);
            let (globals, mut queue) = registry_queue_init::<St>(&conn).ok()?;
            let qh = queue.handle();
            let seat: WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
            let manager: Option<WlDataDeviceManager> = globals.bind(&qh, 1..=3, ()).ok();
            let compositor: Option<WlCompositor> = globals.bind(&qh, 1..=4, ()).ok();
            let shm: Option<WlShm> = globals.bind(&qh, 1..=1, ()).ok();
            manager.as_ref()?;
            let mut st = St {
                serial: 0,
                pressed: false,
                has_pointer: false,
                payload: Arc::new(Vec::new()),
                source_active: false,
                ended: false,
                surface_id: None,
                dropped: Arc::new(Mutex::new(Vec::new())),
                offer_has_uri: false,
                over_ours: false,
                last_enter: 0,
                cur_offer: None,
                wake,
            };
            // capacidades del asiento
            queue.roundtrip(&mut st).ok()?;
            if !st.has_pointer {
                return None;
            }
            let pointer = seat.get_pointer(&qh, ());
            let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface as *mut _) }.ok()?;
            let origin = WlSurface::from_id(&conn, id.clone()).ok()?;
            st.surface_id = Some(id);
            let device = manager.as_ref().map(|m| m.get_data_device(&seat, &qh, ()));
            let _ = conn.flush();
            Some(Wl {
                conn,
                queue,
                qh,
                st,
                origin,
                seat,
                manager,
                compositor,
                shm,
                device,
                _pointer: Some(pointer),
                source: None,
                icon: None,
                done: false,
            })
        }

        pub fn pump(&mut self) {
            let _ = self.queue.dispatch_pending(&mut self.st);
            if self.st.ended {
                self.finish();
            }
            // archivos soltados sobre nuestra ventana
            let _ = self.conn.flush();
        }

        fn finish(&mut self) {
            self.st.ended = false;
            self.st.source_active = false;
            if let Some(s) = self.source.take() {
                s.destroy();
            }
            if let Some((s, b)) = self.icon.take() {
                s.destroy();
                b.destroy();
            }
            self.done = true;
            let _ = self.conn.flush();
        }

        pub fn active(&self) -> bool {
            self.st.source_active
        }

        pub fn take_dropped(&mut self) -> Vec<PathBuf> {
            std::mem::take(&mut *self.st.dropped.lock().unwrap())
        }

        fn make_icon(&mut self) -> Option<(WlSurface, WlBuffer)> {
            let (comp, shm) = (self.compositor.as_ref()?, self.shm.as_ref()?);
            let px = icon_pixels();
            let fd = unsafe { libc::memfd_create(c"fb-drag-icon".as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                return None;
            }
            let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
            (&file).write_all(&px).ok()?;
            let pool = shm.create_pool(file.as_fd(), px.len() as i32, &self.qh, ());
            let buf = pool.create_buffer(0, ICON, ICON, ICON * 4, wl_shm::Format::Argb8888, &self.qh, ());
            pool.destroy();
            let surf = comp.create_surface(&self.qh, ());
            surf.attach(Some(&buf), -(ICON / 2), -(ICON / 2));
            surf.damage(0, 0, ICON, ICON);
            Some((surf, buf))
        }

        pub fn start(&mut self, paths: &[PathBuf]) -> bool {
            self.pump();
            if self.st.source_active || !self.st.pressed {
                return false;
            }
            let (Some(mgr), Some(dev)) = (self.manager.clone(), self.device.clone()) else { return false };
            self.st.payload = Arc::new(uri_list(paths));
            let src = mgr.create_data_source(&self.qh, ());
            src.offer(URI.into());
            src.offer("text/plain;charset=utf-8".into());
            src.set_actions(DndAction::Copy | DndAction::Move | DndAction::Ask);
            let icon = self.make_icon();
            dev.start_drag(Some(&src), &self.origin, icon.as_ref().map(|(s, _)| s), self.st.serial);
            if let Some((s, _)) = &icon {
                s.commit();
            }
            self.icon = icon;
            self.source = Some(src);
            self.st.source_active = true;
            self.st.ended = false;
            self.st.pressed = false;
            let _ = self.conn.flush();
            let _ = &self.seat;
            true
        }
    }

}

// ===========================================================================
// X11 (XDND)
// ===========================================================================
mod x {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::CURRENT_TIME;

    pub struct X11 {
        win: u32,
        busy: Arc<AtomicBool>,
        done: Arc<AtomicBool>,
    }

    impl X11 {
        pub fn new(win: u32) -> Option<X11> {
            Some(X11 { win, busy: Arc::new(AtomicBool::new(false)), done: Arc::new(AtomicBool::new(false)) })
        }

        pub fn active(&self) -> bool {
            self.busy.load(Ordering::Acquire)
        }

        pub fn take_done(&mut self) -> bool {
            self.done.swap(false, Ordering::AcqRel)
        }

        pub fn start(&mut self, paths: &[PathBuf]) -> bool {
            if self.active() {
                return false;
            }
            // ¿botón principal pulsado ahora mismo?
            let Ok((conn, screen)) = RustConnection::connect(None) else { return false };
            let root = conn.setup().roots[screen].root;
            let held = conn
                .query_pointer(root)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|q| u16::from(q.mask) & u16::from(KeyButMask::BUTTON1) != 0)
                .unwrap_or(false);
            if !held {
                return false;
            }
            let (busy, done, win) = (self.busy.clone(), self.done.clone(), self.win);
            let payload = uri_list(paths);
            busy.store(true, Ordering::Release);
            std::thread::spawn(move || {
                let _ = run(conn, screen, win, &payload);
                busy.store(false, Ordering::Release);
                done.store(true, Ordering::Release);
            });
            true
        }
    }

    struct Atoms {
        aware: u32,
        enter: u32,
        position: u32,
        status: u32,
        leave: u32,
        drop_: u32,
        finished: u32,
        selection: u32,
        copy: u32,
        uri: u32,
        targets: u32,
        text: u32,
    }

    fn atoms(c: &RustConnection) -> Result<Atoms, Box<dyn std::error::Error>> {
        let names: [&[u8]; 12] = [
            b"XdndAware", b"XdndEnter", b"XdndPosition", b"XdndStatus", b"XdndLeave", b"XdndDrop", b"XdndFinished",
            b"XdndSelection", b"XdndActionCopy", b"text/uri-list", b"TARGETS", b"text/plain",
        ];
        let cookies: Vec<_> = names.iter().map(|n| c.intern_atom(false, n)).collect::<Result<_, _>>()?;
        let mut v = Vec::new();
        for ck in cookies {
            v.push(ck.reply()?.atom);
        }
        Ok(Atoms {
            aware: v[0],
            enter: v[1],
            position: v[2],
            status: v[3],
            leave: v[4],
            drop_: v[5],
            finished: v[6],
            selection: v[7],
            copy: v[8],
            uri: v[9],
            targets: v[10],
            text: v[11],
        })
    }

    fn aware_version(c: &RustConnection, a: &Atoms, w: Window) -> Option<u32> {
        let r = c.get_property(false, w, a.aware, AtomEnum::ATOM, 0, 1).ok()?.reply().ok()?;
        r.value32()?.next()
    }

    /// Ventana debajo del puntero que acepta XDND (recorre desde la raíz).
    fn find_target(c: &RustConnection, a: &Atoms, root: Window, own: Window) -> Option<(Window, u32)> {
        let mut w = root;
        for _ in 0..24 {
            let child = c.query_pointer(w).ok()?.reply().ok()?.child;
            if child == 0 {
                return None;
            }
            if child == own {
                return None;
            }
            if let Some(v) = aware_version(c, a, child) {
                return Some((child, v.min(5)));
            }
            w = child;
        }
        None
    }

    fn msg(c: &RustConnection, to: Window, ty: u32, data: [u32; 5]) {
        let ev = ClientMessageEvent::new(32, to, ty, data);
        let _ = c.send_event(false, to, EventMask::NO_EVENT, ev);
    }

    fn run(c: RustConnection, screen: usize, own_toplevel: u32, payload: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let root = c.setup().roots[screen].root;
        let a = atoms(&c)?;
        let src = c.generate_id()?;
        c.create_window(0, src, root, -10, -10, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new())?;
        c.set_selection_owner(src, a.selection, CURRENT_TIME)?;
        c.flush()?;

        let mut target: Option<(Window, u32)> = None;
        let mut accepted = false;
        let mut last_pos = (-1i32, -1i32);
        let mut last_sent = Instant::now() - Duration::from_secs(1);
        let started = Instant::now();
        let mut dropped_at: Option<Instant> = None;
        let mut finished = false;

        let serve = |ev: Event, accepted: &mut bool, finished: &mut bool| match ev {
            Event::SelectionRequest(r) if r.selection == a.selection => {
                let prop = if r.property == 0 { r.target } else { r.property };
                let mut ok = true;
                if r.target == a.uri || r.target == a.text {
                    let _ = c.change_property8(PropMode::REPLACE, r.requestor, prop, r.target, payload);
                } else if r.target == a.targets {
                    let _ = c.change_property32(PropMode::REPLACE, r.requestor, prop, AtomEnum::ATOM, &[a.uri, a.text, a.targets]);
                } else {
                    ok = false;
                }
                let n = SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: r.time,
                    requestor: r.requestor,
                    selection: r.selection,
                    target: r.target,
                    property: if ok { prop } else { 0 },
                };
                let _ = c.send_event(false, r.requestor, EventMask::NO_EVENT, n);
                let _ = c.flush();
            }
            Event::ClientMessage(m) if m.type_ == a.status => {
                let d = m.data.as_data32();
                *accepted = d[1] & 1 != 0;
            }
            Event::ClientMessage(m) if m.type_ == a.finished => *finished = true,
            _ => {}
        };

        loop {
            while let Some(ev) = c.poll_for_event()? {
                serve(ev, &mut accepted, &mut finished);
            }
            if finished || started.elapsed() > Duration::from_secs(600) {
                break;
            }
            if let Some(t) = dropped_at {
                // tras soltar: seguimos sirviendo la selección un rato
                if t.elapsed() > Duration::from_secs(8) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let q = c.query_pointer(root)?.reply()?;
            let (x, y) = (q.root_x as i32, q.root_y as i32);
            let held = u16::from(q.mask) & u16::from(KeyButMask::BUTTON1) != 0;
            let now_target = find_target(&c, &a, root, own_toplevel);
            if now_target.map(|t| t.0) != target.map(|t| t.0) {
                if let Some((w, _)) = target {
                    msg(&c, w, a.leave, [src, 0, 0, 0, 0]);
                }
                accepted = false;
                target = now_target;
                last_pos = (-1, -1);
                if let Some((w, v)) = target {
                    msg(&c, w, a.enter, [src, v << 24, a.uri, 0, 0]);
                }
            }
            if let Some((w, _)) = target {
                if (x, y) != last_pos || last_sent.elapsed() > Duration::from_millis(250) {
                    msg(&c, w, a.position, [src, 0, ((x as u32 & 0xffff) << 16) | (y as u32 & 0xffff), CURRENT_TIME, a.copy]);
                    last_pos = (x, y);
                    last_sent = Instant::now();
                }
            }
            if !held {
                match target {
                    Some((w, _)) if accepted => {
                        msg(&c, w, a.drop_, [src, 0, CURRENT_TIME, 0, 0]);
                        dropped_at = Some(Instant::now());
                    }
                    Some((w, _)) => {
                        msg(&c, w, a.leave, [src, 0, 0, 0, 0]);
                        c.flush()?;
                        break;
                    }
                    None => break,
                }
            }
            c.flush()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = c.destroy_window(src);
        let _ = c.flush();
        Ok(())
    }
}
