//! Preset shape outlines, shared by presentations of both kinds.

use vello_cpu::kurbo::{BezPath, Shape};

/// A preset shape's outline in a `w` × `h` box.
pub(crate) fn preset(name: &str, w: f32, h: f32) -> BezPath {
    let (w, h) = (w as f64, h as f64);
    let poly = |pts: &[(f64, f64)]| {
        let mut p = BezPath::new();
        for (i, &(a, b)) in pts.iter().enumerate() {
            if i == 0 {
                p.move_to((a * w, b * h));
            } else {
                p.line_to((a * w, b * h));
            }
        }
        p.close_path();
        p
    };
    let m = w.min(h);
    match name {
        "line" | "straightConnector1" | "bentConnector2" | "bentConnector3" | "curvedConnector3" => {
            let mut p = BezPath::new();
            p.move_to((0.0, 0.0));
            p.line_to((w, h));
            p
        }
        "ellipse" | "flowChartConnector" => vello_cpu::kurbo::Ellipse::new((w / 2.0, h / 2.0), (w / 2.0, h / 2.0), 0.0).to_path(0.1),
        "roundRect" | "flowChartAlternateProcess" => vello_cpu::kurbo::RoundedRect::new(0.0, 0.0, w, h, m * 0.1667).to_path(0.1),
        "triangle" | "flowChartExtract" => poly(&[(0.5, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        "rtTriangle" => poly(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        "diamond" | "flowChartDecision" => poly(&[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)]),
        "parallelogram" | "flowChartInputOutput" => poly(&[(0.25, 0.0), (1.0, 0.0), (0.75, 1.0), (0.0, 1.0)]),
        "trapezoid" => poly(&[(0.25, 0.0), (0.75, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        "pentagon" => poly(&[(0.5, 0.0), (1.0, 0.38), (0.81, 1.0), (0.19, 1.0), (0.0, 0.38)]),
        "hexagon" => poly(&[(0.25, 0.0), (0.75, 0.0), (1.0, 0.5), (0.75, 1.0), (0.25, 1.0), (0.0, 0.5)]),
        "octagon" => poly(&[(0.29, 0.0), (0.71, 0.0), (1.0, 0.29), (1.0, 0.71), (0.71, 1.0), (0.29, 1.0), (0.0, 0.71), (0.0, 0.29)]),
        "homePlate" => poly(&[(0.0, 0.0), (1.0 - (0.5 * m / w).min(1.0), 0.0), (1.0, 0.5), (1.0 - (0.5 * m / w).min(1.0), 1.0), (0.0, 1.0)]),
        "chevron" => {
            let k = (0.5 * m / w).min(0.5);
            poly(&[(0.0, 0.0), (1.0 - k, 0.0), (1.0, 0.5), (1.0 - k, 1.0), (0.0, 1.0), (k, 0.5)])
        }
        "rightArrow" => {
            let k = (0.5 * m / w).min(1.0);
            poly(&[(0.0, 0.25), (1.0 - k, 0.25), (1.0 - k, 0.0), (1.0, 0.5), (1.0 - k, 1.0), (1.0 - k, 0.75), (0.0, 0.75)])
        }
        "leftArrow" => {
            let k = (0.5 * m / w).min(1.0);
            poly(&[(1.0, 0.25), (k, 0.25), (k, 0.0), (0.0, 0.5), (k, 1.0), (k, 0.75), (1.0, 0.75)])
        }
        "upArrow" => {
            let k = (0.5 * m / h).min(1.0);
            poly(&[(0.25, 1.0), (0.25, k), (0.0, k), (0.5, 0.0), (1.0, k), (0.75, k), (0.75, 1.0)])
        }
        "downArrow" => {
            let k = (0.5 * m / h).min(1.0);
            poly(&[(0.25, 0.0), (0.25, 1.0 - k), (0.0, 1.0 - k), (0.5, 1.0), (1.0, 1.0 - k), (0.75, 1.0 - k), (0.75, 0.0)])
        }
        "plus" | "mathPlus" => poly(&[(0.33, 0.0), (0.67, 0.0), (0.67, 0.33), (1.0, 0.33), (1.0, 0.67), (0.67, 0.67), (0.67, 1.0), (0.33, 1.0), (0.33, 0.67), (0.0, 0.67), (0.0, 0.33), (0.33, 0.33)]),
        "snip1Rect" => poly(&[(0.0, 0.0), (1.0 - 0.1667 * m / w, 0.0), (1.0, 0.1667 * m / h), (1.0, 1.0), (0.0, 1.0)]),
        _ => poly(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
    }
}
