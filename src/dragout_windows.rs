//! Arrastre hacia fuera en Windows: `DoDragDrop` con un objeto de datos del shell (CF_HDROP/HIDA).
use raw_window_handle as _;
use std::path::PathBuf;
use windows::core::{implement, HRESULT, PCWSTR};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILFree, SHCreateDataObject, SHParseDisplayName};

#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: windows::core::BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

pub struct Imp {
    done: bool,
}

impl Imp {
    pub fn new(_: &eframe::Frame, _: &egui::Context) -> Option<Imp> {
        unsafe {
            let _ = OleInitialize(None);
        }
        Some(Imp { done: false })
    }

    pub fn pump(&mut self) {}

    pub fn active(&self) -> bool {
        false
    }

    pub fn take_done(&mut self) -> bool {
        std::mem::take(&mut self.done)
    }

    pub fn take_dropped(&mut self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Bloquea hasta que termina el arrastre (bucle modal de OLE).
    pub fn start(&mut self, paths: &[PathBuf]) -> bool {
        use std::os::windows::ffi::OsStrExt;
        unsafe {
            let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
            for p in paths {
                let w: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
                let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
                if SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                    pidls.push(pidl);
                }
            }
            if pidls.is_empty() {
                return false;
            }
            let consts: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
            let data: windows::core::Result<windows::Win32::System::Com::IDataObject> = SHCreateDataObject(None, Some(&consts), None);
            let ok = match data {
                Ok(d) => {
                    let src: IDropSource = Source.into();
                    let mut effect = DROPEFFECT(0);
                    let _ = DoDragDrop(&d, &src, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK, &mut effect);
                    true
                }
                Err(_) => false,
            };
            for p in pidls {
                ILFree(Some(p));
            }
            self.done = ok;
            ok
        }
    }
}
