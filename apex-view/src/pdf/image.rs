//! Image XObjects to PNG (or the JPEG they hold).

use super::file::PdfFile;
use super::obj::Obj;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Space {
    Gray,
    Rgb,
    Cmyk,
    /// Separation or DeviceN: tints of ink.
    Tint,
    Indexed,
    Pattern,
}

type IndexedParts = Option<(Obj, usize, Vec<u8>)>;

/// A colour space's kind, number of components and, for indexed spaces,
/// the base space, highest index and lookup table.
pub fn space(f: &PdfFile, cs: &Obj) -> (Space, usize, IndexedParts) {
    space_in(f, cs, 0)
}

/// `space` `depth` spaces down: one naming itself as its base stops.
fn space_in(f: &PdfFile, cs: &Obj, depth: usize) -> (Space, usize, IndexedParts) {
    if depth > 8 {
        return (Space::Rgb, 3, None);
    }
    match f.resolve(cs) {
        Obj::Name(n) => match &*n {
            "DeviceRGB" | "RGB" | "CalRGB" => (Space::Rgb, 3, None),
            "DeviceCMYK" | "CMYK" => (Space::Cmyk, 4, None),
            "Pattern" => (Space::Pattern, 1, None),
            _ => (Space::Gray, 1, None),
        },
        Obj::Array(a) => {
            let kind = a.first().and_then(Obj::name).unwrap_or("");
            match kind {
                "ICCBased" => {
                    let n = a.get(1).map(|o| f.resolve(o)).and_then(|o| o.dict().and_then(|d| f.key_num(d, "N"))).unwrap_or(3.0) as usize;
                    match n {
                        1 => (Space::Gray, 1, None),
                        4 => (Space::Cmyk, 4, None),
                        _ => (Space::Rgb, 3, None),
                    }
                }
                "CalRGB" | "Lab" => (Space::Rgb, 3, None),
                "CalGray" => (Space::Gray, 1, None),
                "Indexed" | "I" => {
                    let base = a.get(1).cloned().unwrap_or(Obj::Null);
                    let hival = a.get(2).and_then(|o| f.resolve(o).int()).unwrap_or(255).clamp(0, 255) as usize;
                    let lookup = match a.get(3).map(|o| f.resolve(o)) {
                        Some(Obj::Str(s)) => s,
                        Some(Obj::Stream(d, raw)) => f.decode(&d, &raw).data,
                        _ => Vec::new(),
                    };
                    (Space::Indexed, 1, Some((base, hival, lookup)))
                }
                "Separation" => (Space::Tint, 1, None),
                // At least one component: none would split the samples by zero; at
                // most the 32 the format allows.
                "DeviceN" => (Space::Tint, a.get(1).map(|o| f.resolve(o)).and_then(|o| o.array().map(<[Obj]>::len)).unwrap_or(1).clamp(1, 32), None),
                "Pattern" => (Space::Pattern, 1, None),
                _ => space_in(f, a.first().unwrap_or(&Obj::Null), depth + 1),
            }
        }
        _ => (Space::Gray, 1, None),
    }
}

/// Components (0 to 1) as 8-bit RGB.
pub fn to_rgb(kind: Space, c: &[f64]) -> [u8; 3] {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = |i: usize| c.get(i).copied().unwrap_or(0.0);
    match kind {
        Space::Rgb => [b(g(0)), b(g(1)), b(g(2))],
        Space::Cmyk => {
            let k = g(3);
            [b((1.0 - g(0)) * (1.0 - k)), b((1.0 - g(1)) * (1.0 - k)), b((1.0 - g(2)) * (1.0 - k))]
        }
        Space::Tint => {
            let t = c.iter().copied().fold(0.0, f64::max);
            [b(1.0 - t); 3]
        }
        Space::Pattern => [128; 3],
        _ => [b(g(0)); 3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::obj::Dict;
    use std::collections::HashMap;

    #[test]
    fn a_space_naming_itself_stops() {
        let mut objs = HashMap::new();
        objs.insert(1, Obj::Array(vec![Obj::Ref(1, 0)]));
        let f = PdfFile::of_objects(objs, Dict(Vec::new()));
        assert_eq!(space(&f, &Obj::Ref(1, 0)).1, 3);
    }
}
