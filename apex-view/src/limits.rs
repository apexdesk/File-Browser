/// Bounds on what one document may take. Past them a document or page
/// fails with [`crate::Error::TooLarge`] (or is shown cut short, where
/// that is what a viewer would do).
#[derive(Clone, Debug)]
pub struct Limits {
    /// Bytes of the file itself.
    pub max_file_bytes: u64,
    /// Bytes any one part of a package may inflate to.
    pub max_part_bytes: u64,
    /// Bytes all the parts read from a package may inflate to.
    pub max_unpacked_bytes: u64,
    /// How many times larger than its compressed size a part may inflate.
    pub max_ratio: u64,
    /// How deep XML elements may nest.
    pub max_xml_depth: usize,
    /// Pixels of any one picture decoded.
    pub max_image_pixels: u64,
    /// Width of a page drawn, in pixels.
    pub max_render_width: u32,
    /// Pixels of a page drawn.
    pub max_render_pixels: u64,
    /// Pages a document may have (more are left out).
    pub max_pages: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_file_bytes: 512 << 20,
            max_part_bytes: 256 << 20,
            max_unpacked_bytes: 1 << 30,
            max_ratio: 1000,
            max_xml_depth: 256,
            max_image_pixels: 100_000_000,
            max_render_width: 8192,
            max_render_pixels: 100_000_000,
            max_pages: 10_000,
        }
    }
}
