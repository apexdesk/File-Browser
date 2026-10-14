//! Arrastre hacia fuera en macOS: `NSView beginDraggingSession` con un `NSDraggingSource` propio.
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSEventType,
    NSPasteboardWriting, NSView, NSWorkspace,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

declare_class!(
    struct Source;

    unsafe impl ClassType for Source {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FBDragSource";
    }

    impl DeclaredClass for Source {
        type Ivars = Arc<AtomicBool>;
    }

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSDraggingSource for Source {
        #[method(draggingSession:sourceOperationMaskForDraggingContext:)]
        fn source_mask(&self, _session: &NSDraggingSession, _ctx: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic
        }

        #[method(draggingSession:endedAtPoint:operation:)]
        fn ended(&self, _session: &NSDraggingSession, _p: NSPoint, _op: NSDragOperation) {
            self.ivars().store(false, Ordering::Release);
            self.ivars_done();
        }
    }
);

impl Source {
    fn ivars_done(&self) {
        DONE.store(true, Ordering::Release);
    }

    fn new(mtm: MainThreadMarker, active: Arc<AtomicBool>) -> Retained<Source> {
        let this = mtm.alloc::<Source>().set_ivars(active);
        unsafe { msg_send_id![super(this), init] }
    }
}

static DONE: AtomicBool = AtomicBool::new(false);

pub struct Imp {
    view: usize,
    active: Arc<AtomicBool>,
    source: Option<Retained<Source>>,
    session: Option<Retained<NSDraggingSession>>,
}

impl Imp {
    pub fn new(frame: &eframe::Frame, _ctx: &egui::Context) -> Option<Imp> {
        match frame.window_handle().ok()?.as_raw() {
            RawWindowHandle::AppKit(h) => Some(Imp {
                view: h.ns_view.as_ptr() as usize,
                active: Arc::new(AtomicBool::new(false)),
                source: None,
                session: None,
            }),
            _ => None,
        }
    }

    pub fn pump(&mut self) {}

    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn take_done(&mut self) -> bool {
        let d = DONE.swap(false, Ordering::AcqRel);
        if d {
            self.source = None;
            self.session = None;
        }
        d
    }

    pub fn take_dropped(&mut self) -> Vec<PathBuf> {
        Vec::new()
    }

    pub fn start(&mut self, paths: &[PathBuf]) -> bool {
        if self.active() {
            return false;
        }
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let view: &NSView = unsafe { &*(self.view as *const NSView) };
        let app = NSApplication::sharedApplication(mtm);
        let Some(window) = view.window() else { return false };
        // Evento de ratón para iniciar la sesión: el actual si lo es, o uno sintético en la posición del puntero.
        let event: Retained<NSEvent> = match unsafe { app.currentEvent() } {
            Some(e) if matches!(unsafe { e.r#type() }, NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged) => e,
            _ => {
                let loc = unsafe { window.mouseLocationOutsideOfEventStream() };
                let Some(e) = (unsafe {
                    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                        NSEventType::LeftMouseDragged,
                        loc,
                        objc2_app_kit::NSEventModifierFlags::empty(),
                        0.0,
                        window.windowNumber(),
                        None,
                        0,
                        1,
                        1.0,
                    )
                }) else {
                    return false;
                };
                e
            }
        };
        let at = unsafe { view.convertPoint_fromView(event.locationInWindow(), None) };
        let ws = unsafe { NSWorkspace::sharedWorkspace() };
        let mut items = Vec::new();
        for (i, p) in paths.iter().take(32).enumerate() {
            let s = NSString::from_str(&p.to_string_lossy());
            let url = unsafe { NSURL::fileURLWithPath(&s) };
            let writer: &ProtocolObject<dyn NSPasteboardWriting> = ProtocolObject::from_ref(&*url);
            let item = unsafe { NSDraggingItem::initWithPasteboardWriter(mtm.alloc(), writer) };
            let icon = unsafe { ws.iconForFile(&s) };
            let off = i as f64 * 4.0;
            let frame = NSRect::new(NSPoint::new(at.x - 24.0 + off, at.y - 24.0 - off), NSSize::new(48.0, 48.0));
            unsafe { item.setDraggingFrame_contents(frame, Some(&icon)) };
            items.push(item);
        }
        if items.is_empty() {
            return false;
        }
        let arr = NSArray::from_vec(items);
        let source = Source::new(mtm, self.active.clone());
        DONE.store(false, Ordering::Release);
        let session = unsafe { view.beginDraggingSessionWithItems_event_source(&arr, &event, ProtocolObject::from_ref(&*source)) };
        self.active.store(true, Ordering::Release);
        self.source = Some(source);
        self.session = Some(session);
        true
    }
}
