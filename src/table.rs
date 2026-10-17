//! Tablas en la vista previa: CSV/TSV y bases SQLite. Cabecera fija, desplazamiento en ambos ejes
//! y sólo se dibujan las filas visibles.
use crate::theme::Pal;
use egui::{pos2, vec2, Align2, CornerRadius, FontId, Rect, Stroke};

const MAX_ROWS: usize = 50_000;
const MAX_COLS: usize = 200;
const ROW_H: f32 = 24.0;
const HEAD_H: f32 = 28.0;

#[derive(Default)]
pub struct Table {
    pub head: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

/// Separador más probable según las primeras líneas.
fn delimiter(text: &str) -> char {
    let lines: Vec<&str> = text.lines().take(8).collect();
    [',', ';', '\t', '|']
        .into_iter()
        .max_by_key(|d| {
            let counts: Vec<usize> = lines.iter().map(|l| l.matches(*d).count()).collect();
            let first = counts.first().copied().unwrap_or(0);
            if first > 0 && counts.iter().all(|c| *c == first) { first * 10 + 5 } else { counts.iter().min().copied().unwrap_or(0) }
        })
        .unwrap_or(',')
}

pub fn csv(text: &str) -> Table {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let d = delimiter(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut truncated = false;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(if c == '\n' || c == '\r' { ' ' } else { c });
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == d => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                if rows.len() >= MAX_ROWS {
                    truncated = true;
                    break;
                }
            }
            c => field.push(c),
        }
    }
    if !truncated && (!field.is_empty() || !row.is_empty()) {
        row.push(field);
        rows.push(row);
    }
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0).min(MAX_COLS);
    for r in &mut rows {
        r.truncate(width);
        r.resize(width, String::new());
    }
    let head = if rows.is_empty() { Vec::new() } else { rows.remove(0) };
    Table { head, rows, truncated }
}

pub struct TableView {
    pub tables: Vec<(String, Table)>,
    cur: usize,
    scroll: f32,
    scroll_x: f32,
    list_scroll: f32,
    widths: Vec<Vec<f32>>,
}

impl TableView {
    pub fn new(tables: Vec<(String, Table)>) -> TableView {
        let widths = tables
            .iter()
            .map(|(_, t)| {
                (0..t.head.len())
                    .map(|c| {
                        let mut m = t.head[c].chars().count();
                        for r in t.rows.iter().take(150) {
                            m = m.max(r[c].chars().count());
                        }
                        (m as f32 * 7.4 + 22.0).clamp(54.0, 340.0)
                    })
                    .collect()
            })
            .collect();
        TableView { tables, cur: 0, scroll: 0.0, scroll_x: 0.0, list_scroll: 0.0, widths }
    }

    pub fn draw(&mut self, ctx: &egui::Context, p: &egui::Painter, area: Rect, pal: &Pal, hot: bool, pointer: Option<egui::Pos2>, clicked: bool) {
        p.rect_filled(area, CornerRadius::same(10), pal.bg);
        let mut main = area;
        // lista de tablas (bases de datos)
        if self.tables.len() > 1 {
            let lw = 190.0f32.min(area.width() * 0.35);
            let list = Rect::from_min_max(area.min, pos2(area.left() + lw, area.bottom()));
            main = Rect::from_min_max(pos2(list.right(), area.top()), area.max);
            let lp = p.with_clip_rect(list);
            lp.rect_filled(list, CornerRadius::same(10), pal.surface);
            let in_list = pointer.map(|q| list.contains(q)).unwrap_or(false);
            let total = self.tables.len() as f32 * 28.0 + 16.0;
            if hot && in_list {
                self.list_scroll = (self.list_scroll - ctx.input(|i| i.smooth_scroll_delta.y)).clamp(0.0, (total - list.height()).max(0.0));
            }
            for (i, (name, _)) in self.tables.iter().enumerate() {
                let r = Rect::from_min_size(pos2(list.left() + 6.0, list.top() + 8.0 + i as f32 * 28.0 - self.list_scroll), vec2(lw - 12.0, 26.0));
                if r.bottom() < list.top() || r.top() > list.bottom() {
                    continue;
                }
                let over = pointer.map(|q| r.contains(q) && list.contains(q)).unwrap_or(false);
                if i == self.cur {
                    lp.rect_filled(r, CornerRadius::same(7), pal.sel);
                } else if over {
                    lp.rect_filled(r, CornerRadius::same(7), pal.hover);
                }
                let mut n = name.clone();
                if n.chars().count() > 24 {
                    n = n.chars().take(23).collect::<String>() + "…";
                }
                lp.text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, n, FontId::proportional(13.0), if i == self.cur { pal.accent } else { pal.text });
                if over && clicked && i != self.cur {
                    self.cur = i;
                    self.scroll = 0.0;
                    self.scroll_x = 0.0;
                }
            }
        }
        let Some((_, t)) = self.tables.get(self.cur) else { return };
        let widths = &self.widths[self.cur];
        let gutter = 46.0;
        let total_w: f32 = gutter + widths.iter().sum::<f32>();
        let total_h = t.rows.len() as f32 * ROW_H;
        let body = Rect::from_min_max(pos2(main.left(), main.top() + HEAD_H), main.max);
        let in_main = pointer.map(|q| main.contains(q)).unwrap_or(false);
        let (dx, dy, shift) = ctx.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y, i.modifiers.shift));
        if hot && in_main {
            let (ax, ay) = if shift { (dx + dy, 0.0) } else { (dx, dy) };
            self.scroll_x = (self.scroll_x - ax).clamp(0.0, (total_w - main.width()).max(0.0));
            self.scroll = (self.scroll - ay).clamp(0.0, (total_h - body.height()).max(0.0));
        }
        crate::rich::scroll_input(ctx, body, total_h, &mut self.scroll, false);
        let cp = p.with_clip_rect(main);
        let font = FontId::proportional(13.0);
        // columnas visibles
        let mut xs = Vec::with_capacity(widths.len());
        let mut x = main.left() + gutter - self.scroll_x;
        for w in widths {
            xs.push(x);
            x += w;
        }
        let first = (self.scroll / ROW_H) as usize;
        let n = (body.height() / ROW_H) as usize + 2;
        for (k, row) in t.rows.iter().enumerate().skip(first).take(n) {
            let y = body.top() + k as f32 * ROW_H - self.scroll;
            if k % 2 == 1 {
                cp.rect_filled(Rect::from_min_size(pos2(main.left(), y), vec2(main.width(), ROW_H)), 0.0, pal.surface.gamma_multiply(0.55));
            }
            cp.text(pos2(main.left() + gutter - 8.0, y + ROW_H / 2.0), Align2::RIGHT_CENTER, k + 1, FontId::monospace(11.0), pal.subtle.gamma_multiply(0.7));
            for (c, cell) in row.iter().enumerate() {
                let (cx, w) = (xs[c], widths[c]);
                if cx + w < main.left() + gutter || cx > main.right() || cell.is_empty() {
                    continue;
                }
                let maxc = ((w - 14.0) / 7.2).max(1.0) as usize;
                let txt: String = if cell.chars().count() > maxc { cell.chars().take(maxc.saturating_sub(1)).collect::<String>() + "…" } else { cell.clone() };
                let num = cell.parse::<f64>().is_ok();
                let (pos, al) = if num { (pos2(cx + w - 8.0, y + ROW_H / 2.0), Align2::RIGHT_CENTER) } else { (pos2(cx + 8.0, y + ROW_H / 2.0), Align2::LEFT_CENTER) };
                cp.with_clip_rect(Rect::from_min_max(pos2((main.left() + gutter).max(cx), y), pos2((cx + w).min(main.right()), y + ROW_H))).text(pos, al, txt, font.clone(), pal.text);
            }
        }
        // cabecera fija
        cp.rect_filled(Rect::from_min_size(main.min, vec2(main.width(), HEAD_H)), CornerRadius::ZERO, pal.panel);
        for (c, h) in t.head.iter().enumerate() {
            let (cx, w) = (xs[c], widths[c]);
            if cx + w < main.left() + gutter || cx > main.right() {
                continue;
            }
            let maxc = ((w - 14.0) / 7.6).max(1.0) as usize;
            let txt: String = if h.chars().count() > maxc { h.chars().take(maxc.saturating_sub(1)).collect::<String>() + "…" } else { h.clone() };
            cp.with_clip_rect(Rect::from_min_max(pos2((main.left() + gutter).max(cx), main.top()), pos2((cx + w).min(main.right()), main.top() + HEAD_H)))
                .text(pos2(cx + 8.0, main.top() + HEAD_H / 2.0), Align2::LEFT_CENTER, txt, FontId::proportional(13.0), pal.accent);
        }
        cp.line_segment([pos2(main.left(), main.top() + HEAD_H), pos2(main.right(), main.top() + HEAD_H)], Stroke::new(1.0, pal.border));
        if t.rows.is_empty() {
            cp.text(body.center(), Align2::CENTER_CENTER, crate::i18n::t("Sin datos"), FontId::proportional(14.0), pal.subtle);
        }
        if t.truncated {
            cp.text(pos2(main.right() - 12.0, main.bottom() - 8.0), Align2::RIGHT_BOTTOM, crate::i18n::t("(vista parcial)"), FontId::proportional(11.5), pal.subtle);
        }
        crate::rich::scrollbar(&cp, body, total_h + 0.0, self.scroll, pal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_and_delimiters() {
        let t = csv("a;b;c\n1;\"x;y\";3\n4;\"he said \"\"hi\"\"\";6\n");
        assert_eq!(t.head, ["a", "b", "c"]);
        assert_eq!(t.rows[0], ["1", "x;y", "3"]);
        assert_eq!(t.rows[1][1], "he said \"hi\"");
        let t = csv("x\ty\n1\t2");
        assert_eq!(t.rows[0], ["1", "2"]);
    }
}
