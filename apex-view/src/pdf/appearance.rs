//! Appearance streams: what an annotation or a form field looks like,
//! drawn once so that every viewer shows the same. Text uses Helvetica
//! (one of the standard fonts every viewer has) in WinAnsi, so characters
//! outside Western European ones show as "?"; the annotation's own text
//! keeps them all.


use super::obj::{Dict, Obj};
use super::tables;

/// Font resource names.
pub const HELV: &str = "Helv";
pub const HELV_BOLD: &str = "HeBo";

/// The resources with Helvetica and Helvetica-Bold (and a graphics state
/// `GS0` with opacity `alpha` and blend mode `blend`, when given).
pub fn resources(alpha: Option<f64>, blend: Option<&str>) -> Dict {
    let font = |base: &str| Obj::Dict(Dict::of(vec![("Type", Obj::name_of("Font")), ("Subtype", Obj::name_of("Type1")), ("BaseFont", Obj::name_of(base)), ("Encoding", Obj::name_of("WinAnsiEncoding"))]));
    let mut r = Dict::of(vec![("Font", Obj::Dict(Dict::of(vec![(HELV, font("Helvetica")), (HELV_BOLD, font("Helvetica-Bold"))])))]);
    if alpha.is_some() || blend.is_some() {
        let mut gs = Dict::of(vec![("Type", Obj::name_of("ExtGState"))]);
        if let Some(a) = alpha {
            gs.set("CA", Obj::Real(a));
            gs.set("ca", Obj::Real(a));
        }
        if let Some(b) = blend {
            gs.set("BM", Obj::name_of(b));
        }
        r.set("ExtGState", Obj::Dict(Dict::of(vec![("GS0", Obj::Dict(gs))])));
    }
    r
}

pub fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// A number for a content stream.
pub fn n(v: f64) -> String {
    let v = round(v);
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// WinAnsi code of a character (`?` when it has none).
pub fn win_ansi(c: char) -> u8 {
    let u = c as u32;
    if (32..127).contains(&u) {
        return u as u8;
    }
    tables::WIN_ANSI.iter().position(|&x| x as u32 == u && x != 0).map_or(b'?', |i| i as u8)
}

/// The text as a WinAnsi string operand.
pub fn string(s: &str) -> String {
    let mut out = String::from("(");
    for c in s.chars() {
        match win_ansi(c) {
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\\' => out.push_str("\\\\"),
            b @ 32..=126 => out.push(b as char),
            b => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
    out
}

