//! Shell de Windows: menú contextual nativo (el del Explorador, con las extensiones instaladas como
//! 7-Zip, antivirus, nube…) y etiqueta de volumen.
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use windows::core::{w, Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILFindLastID, ILFree, IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, SHBindToParent, SHParseDisplayName,
    CMF_EXTENDEDVERBS, CMF_NORMAL, CMINVOKECOMMANDINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos, RegisterClassW, SetForegroundWindow,
    TrackPopupMenuEx, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM,
    WM_MENUCHAR, WNDCLASSW, WS_POPUP,
};

fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Etiqueta que el usuario dio al volumen en Windows («Datos», «Cryptomator»…), si tiene.
pub fn volume_label(root: &Path) -> Option<String> {
    let w = wide(root);
    let mut name = [0u16; 261];
    unsafe {
        GetVolumeInformationW(PCWSTR(w.as_ptr()), Some(&mut name), None, None, None, None).ok()?;
    }
    let n = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let s = String::from_utf16_lossy(&name[..n]);
    (!s.trim().is_empty()).then_some(s)
}

thread_local! {
    /// Menú que está abierto ahora: la ventana propietaria le reenvía los mensajes de los submenús.
    static OPEN: RefCell<Option<IContextMenu>> = const { RefCell::new(None) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
            let handled = OPEN.with(|o| {
                let cm = o.borrow().clone()?;
                if let Ok(c3) = cm.cast::<IContextMenu3>() {
                    let mut res = LRESULT(0);
                    if c3.HandleMenuMsg2(msg, wp, lp, Some(&mut res)).is_ok() {
                        return Some(res);
                    }
                }
                if msg != WM_MENUCHAR {
                    if let Ok(c2) = cm.cast::<IContextMenu2>() {
                        if c2.HandleMenuMsg(msg, wp, lp).is_ok() {
                            return Some(LRESULT(0));
                        }
                    }
                }
                None
            });
            if let Some(r) = handled {
                return r;
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

/// Ventana oculta que hace de propietaria del menú (recibe los mensajes de los submenús).
unsafe fn owner_window() -> Option<HWND> {
    unsafe {
        let inst = GetModuleHandleW(None).ok()?;
        let class = w!("FileBrowserShellMenuOwner");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc); // si ya estaba registrada devuelve 0 y no importa
        CreateWindowExW(WINDOW_EX_STYLE(0), class, w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(inst.into()), None).ok()
    }
}

/// Muestra en el puntero el menú contextual del Explorador y ejecuta lo elegido. Con `background` es el
/// menú del hueco de la carpeta `paths[0]`; si no, el de esos archivos (todos de la misma carpeta).
/// Bloquea hasta que se cierra el menú. `false` si no se pudo mostrar.
pub fn context_menu(paths: &[PathBuf], background: bool) -> bool {
    if paths.is_empty() {
        return false;
    }
    unsafe { context_menu_inner(paths, background).is_some() }
}

unsafe fn context_menu_inner(paths: &[PathBuf], background: bool) -> Option<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        // el menú es de una sola carpeta: los de otra (resultados de búsqueda) se dejan fuera
        let dir = paths[0].parent();
        for p in paths.iter().filter(|p| p.parent() == dir) {
            let w = wide(p);
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            if SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                pidls.push(pidl);
            }
            if background {
                break;
            }
        }
        let owner = owner_window();
        let hwnd = owner.unwrap_or_default();
        let res = (|| -> Option<()> {
            let first = *pidls.first()?;
            let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
            let parent: IShellFolder = SHBindToParent(first, Some(&mut child)).ok()?;
            let cm: IContextMenu = if background {
                let folder: IShellFolder = parent.BindToObject(child, None).ok()?;
                folder.CreateViewObject(hwnd).ok()?
            } else {
                let children: Vec<*const ITEMIDLIST> = pidls.iter().map(|&p| ILFindLastID(p) as *const ITEMIDLIST).collect();
                parent.GetUIObjectOf(hwnd, &children, None).ok()?
            };
            let menu = CreatePopupMenu().ok()?;
            let shift = GetKeyState(0x10) < 0;
            let flags = CMF_NORMAL | if shift { CMF_EXTENDEDVERBS } else { 0 };
            let out = (|| -> Option<()> {
                if cm.QueryContextMenu(menu, 0, 1, 0x7FFF, flags).is_err() {
                    return None;
                }
                OPEN.with(|o| *o.borrow_mut() = Some(cm.clone()));
                let mut pt = POINT::default();
                GetCursorPos(&mut pt).ok()?;
                let _ = SetForegroundWindow(hwnd);
                let id = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0;
                OPEN.with(|o| *o.borrow_mut() = None);
                if id > 0 {
                    let info = CMINVOKECOMMANDINFO {
                        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                        hwnd,
                        lpVerb: PCSTR((id - 1) as usize as *const u8),
                        nShow: SW_SHOWNORMAL.0,
                        ..Default::default()
                    };
                    cm.InvokeCommand(&info).ok()?;
                }
                Some(())
            })();
            OPEN.with(|o| *o.borrow_mut() = None);
            let _ = DestroyMenu(menu);
            out
        })();
        for p in pidls {
            ILFree(Some(p));
        }
        if let Some(w) = owner {
            let _ = DestroyWindow(w);
        }
        res
    }
}
