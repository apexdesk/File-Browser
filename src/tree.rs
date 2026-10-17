//! Árbol plegable de JSON y XML en la vista previa.
use crate::markup::{self, Json, Tok};
use crate::theme::Pal;
use egui::{pos2, vec2, Align2, CornerRadius, FontId, Rect};

const MAX_NODES: usize = 400_000;
const ROW_H: f32 = 21.0;

pub struct Node {
    label: String,
    value: String,
    kids: Vec<u32>,
    /// 0 valor simple, 1 texto, 2 número/bool/null, 3 contenedor
    kind: u8,
}

pub struct Tree {
    nodes: Vec<Node>,
}

fn add(nodes: &mut Vec<Node>, label: String, value: String, kind: u8) -> u32 {
    nodes.push(Node { label, value, kids: Vec::new(), kind });
    (nodes.len() - 1) as u32
}

fn json_into(nodes: &mut Vec<Node>, label: String, j: &Json) -> u32 {
    if nodes.len() >= MAX_NODES {
        return add(nodes, label, "…".into(), 0);
    }
    match j {
        Json::Null => add(nodes, label, "null".into(), 2),
        Json::Bool(b) => add(nodes, label, b.to_string(), 2),
        Json::Num(_, s) => add(nodes, label, s.clone(), 2),
        Json::Str(s) => {
            let v: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(400).collect();
            add(nodes, label, format!("\"{v}\""), 1)
        }
        Json::Arr(v) => {
            let id = add(nodes, label, format!("[ ] {}", v.len()), 3);
            let kids: Vec<u32> = v.iter().enumerate().map(|(i, x)| json_into(nodes, format!("[{i}]"), x)).collect();
            nodes[id as usize].kids = kids;
            id
        }
        Json::Obj(v) => {
            let id = add(nodes, label, format!("{{ }} {}", v.len()), 3);
            let kids: Vec<u32> = v.iter().map(|(k, x)| json_into(nodes, k.clone(), x)).collect();
            nodes[id as usize].kids = kids;
            id
        }
    }
}

pub fn from_json(text: &str) -> Option<Tree> {
    let j = markup::parse_json(text)?;
    let mut nodes = Vec::new();
    json_into(&mut nodes, "$".into(), &j);
    Some(Tree { nodes })
}

pub fn from_xml(text: &str) -> Option<Tree> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut roots = 0;
    for tok in markup::tokens(text, false) {
        if nodes.len() >= MAX_NODES {
            break;
        }
        match tok {
            Tok::Open { name, attrs, empty } => {
                let a: String = attrs.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect::<Vec<_>>().join("  ");
                let id = add(&mut nodes, name, a, 3);
                match stack.last() {
                    Some(&p) => nodes[p as usize].kids.push(id),
                    None => roots += 1,
                }
                if !empty {
                    stack.push(id);
                }
            }
            Tok::Close(_) => {
                stack.pop();
            }
            Tok::Text(t) => {
                let t = markup::decode_entities(t.trim());
                if let (false, Some(&p)) = (t.is_empty(), stack.last()) {
                    let t: String = t.chars().map(|c| if c.is_control() { ' ' } else { c }).take(400).collect();
                    let id = add(&mut nodes, String::new(), t, 1);
                    nodes[p as usize].kids.push(id);
                }
            }
        }
    }
    if roots == 0 {
        return None;
    }
    // se asume que la primera raíz es el nodo 0; las demás (raras) cuelgan de un nodo virtual
    Some(Tree { nodes })
}

pub struct TreeView {
    tree: Tree,
    open: std::collections::HashSet<u32>,
    rows: Vec<(u32, u16)>,
    scroll: f32,
}

impl TreeView {
    pub fn new(tree: Tree) -> TreeView {
        let mut v = TreeView { tree, open: Default::default(), rows: Vec::new(), scroll: 0.0 };
        v.open.insert(0);
        // los hijos directos de la raíz que son pequeños se abren
        let kids = v.tree.nodes.first().map(|n| n.kids.clone()).unwrap_or_default();
        for k in kids.into_iter().take(40) {
            if v.tree.nodes[k as usize].kids.len() < 12 {
                v.open.insert(k);
            }
        }
        v.rebuild();
        v
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        let mut stack = vec![(0u32, 0u16)];
        while let Some((id, d)) = stack.pop() {
            if self.rows.len() >= 200_000 {
                break;
            }
            self.rows.push((id, d));
            if self.open.contains(&id) {
                for &k in self.tree.nodes[id as usize].kids.iter().rev() {
                    stack.push((k, d + 1));
                }
            }
        }
    }

    fn expand_all(&mut self, on: bool) {
        self.open.clear();
        if on {
            for (i, n) in self.tree.nodes.iter().enumerate() {
                if !n.kids.is_empty() {
                    self.open.insert(i as u32);
                }
            }
        } else {
            self.open.insert(0);
        }
        self.rebuild();
        self.scroll = 0.0;
    }

    pub fn draw(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, pal: &Pal, hot: bool, pointer: Option<egui::Pos2>, clicked: bool) {
        p.rect_filled(area, CornerRadius::same(10), pal.bg);
        let cp = p.with_clip_rect(area);
        // botones de expandir / contraer arriba a la derecha
        let mut act: Option<bool> = None;
        for (i, (glyph, on)) in [("−", false), ("+", true)].into_iter().enumerate() {
            let r = Rect::from_center_size(pos2(area.right() - 24.0 - i as f32 * 34.0, area.top() + 20.0), vec2(28.0, 26.0));
            let over = pointer.map(|q| r.contains(q)).unwrap_or(false);
            cp.rect_filled(r, CornerRadius::same(7), if over { pal.hover } else { pal.surface });
            cp.text(r.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(17.0), pal.text);
            if over && clicked {
                act = Some(on);
            }
        }
        if let Some(on) = act {
            self.expand_all(on);
            return;
        }
        let top = area.top() + 40.0;
        let body = Rect::from_min_max(pos2(area.left(), top), area.max);
        let total = self.rows.len() as f32 * ROW_H + 12.0;
        crate::rich::scroll_input(ctx, body, total, &mut self.scroll, hot);
        let first = (self.scroll / ROW_H) as usize;
        let n = (body.height() / ROW_H) as usize + 2;
        let mono = FontId::monospace(13.0);
        let mut toggle: Option<u32> = None;
        for (k, &(id, depth)) in self.rows.iter().enumerate().skip(first).take(n) {
            let y = top + 6.0 + k as f32 * ROW_H - self.scroll;
            let node = &self.tree.nodes[id as usize];
            let x = area.left() + 14.0 + depth as f32 * 18.0;
            let row = Rect::from_min_size(pos2(area.left() + 4.0, y), vec2(area.width() - 12.0, ROW_H));
            let over = pointer.map(|q| row.contains(q) && body.contains(q)).unwrap_or(false);
            if over {
                cp.rect_filled(row, CornerRadius::same(5), pal.hover);
            }
            let mid = y + ROW_H / 2.0;
            if !node.kids.is_empty() {
                let c = pos2(x + 5.0, mid);
                let pts = if self.open.contains(&id) {
                    vec![c + vec2(-4.0, -2.5), c + vec2(4.0, -2.5), c + vec2(0.0, 3.0)]
                } else {
                    vec![c + vec2(-2.5, -4.0), c + vec2(-2.5, 4.0), c + vec2(3.0, 0.0)]
                };
                cp.add(egui::Shape::convex_polygon(pts, pal.subtle, egui::Stroke::NONE));
                if over && clicked {
                    toggle = Some(id);
                }
            }
            let mut tx = x + 16.0;
            if !node.label.is_empty() {
                let r = cp.text(pos2(tx, mid), Align2::LEFT_CENTER, &node.label, mono.clone(), pal.accent);
                tx = r.right() + 8.0;
            }
            let col = match node.kind {
                1 => pal.text,
                2 => egui::Color32::from_rgb(214, 160, 90),
                _ => pal.subtle,
            };
            if !node.value.is_empty() {
                let max = ((area.right() - tx - 20.0) / 7.8).max(4.0) as usize;
                let v: String = if node.value.chars().count() > max { node.value.chars().take(max - 1).collect::<String>() + "…" } else { node.value.clone() };
                cp.text(pos2(tx, mid), Align2::LEFT_CENTER, v, mono.clone(), col);
            }
        }
        if let Some(id) = toggle {
            if !self.open.remove(&id) {
                self.open.insert(id);
            }
            self.rebuild();
        }
        crate::rich::scrollbar(&cp, body, total, self.scroll, pal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_and_xml_trees() {
        let t = from_json(r#"{"a":[1,2],"b":"x"}"#).unwrap();
        assert_eq!(t.nodes[0].kids.len(), 2);
        assert_eq!(t.nodes[1].kids.len(), 2);
        let t = from_xml("<r a=\"1\"><c>hola</c><d/></r>").unwrap();
        assert_eq!(t.nodes[0].label, "r");
        assert_eq!(t.nodes[0].kids.len(), 2);
        assert_eq!(t.nodes[1].kids.len(), 1);
    }
}
