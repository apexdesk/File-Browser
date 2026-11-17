//! Lo poco de Win32 que la biblioteca estándar no da: identidad de archivo, nombre largo (frente a los
//! nombres cortos 8.3), renombrar sin pisar, eco de la consola y si el proceso está elevado.
//! Declaraciones a mano para no añadir dependencias; todo devuelve `None`/`false` si la llamada falla.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

type Handle = *mut c_void;

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
pub const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_SHARE_ALL: u32 = 0x7;
const FILE_ID_INFO_CLASS: u32 = 18;
const TOKEN_QUERY: u32 = 0x8;
const TOKEN_ELEVATION: u32 = 20;
const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const ENABLE_ECHO_INPUT: u32 = 0x4;

#[repr(C)]
struct FileIdInfo {
    volume: u64,
    id: [u8; 16],
}

#[repr(C)]
#[derive(Default)]
struct ByHandleInfo {
    attrs: u32,
    times: [u32; 6],
    volume: u32,
    size_hi: u32,
    size_lo: u32,
    links: u32,
    idx_hi: u32,
    idx_lo: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileInformationByHandleEx(h: Handle, class: u32, info: *mut c_void, len: u32) -> i32;
    fn GetFileInformationByHandle(h: Handle, info: *mut ByHandleInfo) -> i32;
    fn GetFinalPathNameByHandleW(h: Handle, buf: *mut u16, len: u32, flags: u32) -> u32;
    fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    fn GetStdHandle(n: u32) -> Handle;
    fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
    fn SetConsoleMode(h: Handle, mode: u32) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn CloseHandle(h: Handle) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(p: Handle, access: u32, tok: *mut Handle) -> i32;
    fn GetTokenInformation(tok: Handle, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
}

/// (volumen, identificador) del archivo abierto: igual ⇔ mismo archivo (como dev/ino en Unix).
pub fn file_id(f: &File) -> Option<(u64, u128)> {
    let h = f.as_raw_handle() as Handle;
    let mut i = FileIdInfo { volume: 0, id: [0; 16] };
    // SAFETY: búfer del tamaño que se declara; el handle es válido mientras viva `f`
    if unsafe { GetFileInformationByHandleEx(h, FILE_ID_INFO_CLASS, (&raw mut i).cast(), size_of::<FileIdInfo>() as u32) } != 0 {
        return Some((i.volume, u128::from_le_bytes(i.id)));
    }
    let mut b = ByHandleInfo::default();
    // SAFETY: ídem (Windows anteriores a 8 / sistemas de archivos sin FileIdInfo)
    if unsafe { GetFileInformationByHandle(h, &raw mut b) } != 0 {
        return Some((u64::from(b.volume), (u128::from(b.idx_hi) << 32) | u128::from(b.idx_lo)));
    }
    None
}

/// Abre la entrada misma (sin seguir un enlace o unión en el último componente) solo para consultarla.
fn open_entry(p: &Path) -> io::Result<File> {
    OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).share_mode(FILE_SHARE_ALL).custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(p)
}

/// Identidad de la entrada `p` sin seguir el último componente.
pub fn entry_id(p: &Path) -> Option<(u64, u128)> {
    file_id(&open_entry(p).ok()?)
}

/// Nombre largo real del último componente de `p` (resuelve los nombres cortos 8.3 como `GIT~1`).
pub fn long_name(p: &Path) -> Option<String> {
    let f = open_entry(p).ok()?;
    let mut buf = vec![0u16; 32_768];
    // SAFETY: búfer de `buf.len()` unidades; flags 0 = ruta normalizada con letra de unidad
    let n = unsafe { GetFinalPathNameByHandleW(f.as_raw_handle() as Handle, buf.as_mut_ptr(), buf.len() as u32, 0) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let full = String::from_utf16(&buf[..n]).ok()?;
    full.rsplit('\\').next().filter(|s| !s.is_empty()).map(str::to_string)
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Renombra sin reemplazar nunca un destino existente (sin MOVEFILE_REPLACE_EXISTING; solo en el mismo volumen).
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let (a, b) = (wide(from), wide(to));
    // SAFETY: cadenas UTF-16 terminadas en 0 que viven durante la llamada
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } != 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// ¿Corre el proceso elevado (administrador con UAC aceptado, o cuenta de sistema)?
pub fn is_elevated() -> bool {
    let mut tok: Handle = std::ptr::null_mut();
    // SAFETY: pseudo-handle del proceso actual; `tok` se cierra abajo
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut tok) } == 0 {
        return false;
    }
    let (mut elev, mut ret) = (0u32, 0u32);
    // SAFETY: TOKEN_ELEVATION es un DWORD
    let ok = unsafe { GetTokenInformation(tok, TOKEN_ELEVATION, (&raw mut elev).cast(), 4, &raw mut ret) } != 0;
    // SAFETY: handle abierto arriba
    unsafe { CloseHandle(tok) };
    ok && elev != 0
}

/// Quita el eco de la consola mientras vive (para teclear la contraseña); restaura el modo al soltarse.
pub struct NoEcho(Option<(Handle, u32)>);

impl NoEcho {
    pub fn new() -> NoEcho {
        // SAFETY: si la entrada no es una consola, GetConsoleMode falla y no se toca nada
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            if h.is_null() || GetConsoleMode(h, &raw mut mode) == 0 {
                return NoEcho(None);
            }
            SetConsoleMode(h, mode & !ENABLE_ECHO_INPUT);
            NoEcho(Some((h, mode)))
        }
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        if let Some((h, mode)) = self.0 {
            // SAFETY: restaura el modo guardado en `new`
            unsafe { SetConsoleMode(h, mode) };
        }
    }
}
