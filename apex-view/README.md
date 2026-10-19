# apex-view

View-only renderer of office documents and PDF files, from ApexDesk's own
readers: each page drawn as RGBA pixels, and its text. Nothing is written,
run or fetched; files are untrusted and read within `Limits`.

```rust
let mut doc = apex_view::Document::open_path("report.docx".as_ref())?;
let n = doc.page_count();
let img = doc.render_page(0, 256, &|| false)?; // RGBA8, opaque
let text = doc.page_text(0)?;
```

One feature per format, all on by default: `docx`, `xlsx`, `pptx`, `pdf`,
`odt`, `ods`, `odp`, `rtf`, `doc`, `xls`, `ppt` (`doc` reads Word 97-2003
and Word 6.0/95, `xls` Excel 97-2003 and 5.0/95). Sheets give one page per
sheet, presentations one per slide. `pdf-scans` (off) adds JPEG 2000,
JBIG2 and CCITT pictures. Password-protected files give `Error::Encrypted`.

Size this crate adds to a program that already uses `image` (png, gif,
webp, bmp), `md-5`, `sha2` and `flate2`; Linux x86-64, fat LTO, stripped,
with the profile below:

| Features | Adds |
|---|---|
| every format | 2.99 MB |
| every office format (no `pdf`) | 1.99 MB |
| `docx` `xlsx` `pptx` `odt` `ods` `odp` | 1.62 MB |
| `docx` alone | 1.33 MB |
| `pdf` alone | 2.30 MB |

Pictures other than JPEG are decoded through `image::load_from_memory`,
which such a program has already, so its decoders are not built twice.
At these optimization levels pages are drawn within about 12 % of
`opt-level = 3` (a first page in under 110 ms), so a program using this
crate should add them to its own `Cargo.toml`:

```toml
[profile.release.package]
apex-view.opt-level = "z"
vello_cpu.opt-level = "s"
vello_common.opt-level = "s"
fearless_simd.opt-level = "s"
skrifa.opt-level = "s"
read-fonts.opt-level = "s"
font-types.opt-level = "s"
kurbo.opt-level = "s"
peniko.opt-level = "s"
color.opt-level = "s"
encoding_rs.opt-level = "s"
zune-jpeg.opt-level = "s"
zune-core.opt-level = "s"
```

A program using this crate needs the patched `read-fonts` too (see
`vendor/README.md`): add to its own `Cargo.toml`

```toml
[patch.crates-io]
read-fonts = { path = "<apex-view>/vendor/read-fonts" }
```

Licence: PolyForm Strict 1.0.0 (see LICENSE).
