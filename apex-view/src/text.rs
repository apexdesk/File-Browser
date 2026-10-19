//! The text of a page: characters placed by the readers gathered into runs
//! (words and the spaces between them, on one line), and runs into plain
//! text.

/// Text on a page: where it starts and its box, as fractions of the
/// page's width and height from the top-left corner.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A character as a reader places it, in points from the top-left corner
/// (`y` is its baseline).
pub(crate) struct Glyph {
    pub ch: char,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub size: f32,
}

/// Characters in the order they were drawn gathered into runs: one run
/// goes on while characters follow on the same baseline.
pub(crate) fn runs(glyphs: impl IntoIterator<Item = Glyph>, page_w: f32, page_h: f32) -> Vec<TextRun> {
    let (pw, ph) = (page_w.max(1.0), page_h.max(1.0));
    let mut out = Vec::new();
    // The run being built: text, left, baseline, right, size.
    let mut cur: Option<(String, f32, f32, f32, f32)> = None;
    let flush = |cur: &mut Option<(String, f32, f32, f32, f32)>, out: &mut Vec<TextRun>| {
        if let Some((text, x0, base, x1, size)) = cur.take()
            && !text.trim().is_empty()
        {
            out.push(TextRun { text, x: x0 / pw, y: (base - size * 0.8) / ph, width: (x1 - x0).max(0.0) / pw, height: size / ph });
        }
    };
    for g in glyphs {
        if !(g.x.is_finite() && g.y.is_finite() && g.w.is_finite() && g.size.is_finite()) {
            continue;
        }
        let size = g.size.abs().max(0.1);
        if let Some((text, _, base, x1, csize)) = &mut cur {
            let same_line = (g.y - *base).abs() < 0.4 * size.max(*csize);
            let gap = g.x - *x1;
            if same_line && gap > -0.5 * size && gap < 3.0 * size {
                if gap > 0.12 * size && !text.ends_with(' ') && g.ch != ' ' {
                    text.push(' ');
                }
                text.push(g.ch);
                *x1 = x1.max(g.x + g.w);
                *csize = csize.max(size);
                continue;
            }
            flush(&mut cur, &mut out);
        }
        cur = Some((g.ch.to_string(), g.x, g.y, g.x + g.w, size));
    }
    flush(&mut cur, &mut out);
    out
}

/// Runs as plain text: runs on one line joined by a space, lines by '\n',
/// top to bottom (left to right within a line).
pub(crate) fn plain(runs: &[TextRun]) -> String {
    let mut lines: Vec<(f32, f32, Vec<&TextRun>)> = Vec::new();
    for r in runs {
        let mid = r.y + r.height / 2.0;
        match lines.iter_mut().find(|(top, bottom, _)| mid > *top && mid < *bottom) {
            Some((_, _, l)) => l.push(r),
            None => lines.push((r.y, r.y + r.height, vec![r])),
        }
    }
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = String::new();
    for (_, _, mut l) in lines {
        l.sort_by(|a, b| a.x.total_cmp(&b.x));
        let line: Vec<&str> = l.iter().map(|r| r.text.trim()).collect();
        out.push_str(&line.join(" "));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(s: &str, x: f32, y: f32) -> Vec<Glyph> {
        s.chars().enumerate().map(|(i, ch)| Glyph { ch, x: x + i as f32 * 6.0, y, w: 6.0, size: 12.0 }).collect()
    }

    #[test]
    fn words_on_a_line_make_one_run_and_lines_stack() {
        let mut g = word("Hello", 10.0, 100.0);
        g.extend(word("world", 50.0, 100.0));
        g.extend(word("Second", 10.0, 120.0));
        let r = runs(g, 200.0, 200.0);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].text, "Hello world");
        assert_eq!(plain(&r), "Hello world\nSecond\n");
    }
}
