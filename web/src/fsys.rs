//! Lo mínimo de `fsys` que necesita `theme.rs` (compartido con el gestor de escritorio).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Folder,
    Image,
    Video,
    Audio,
    Text,
    Archive,
    Doc,
    Other,
}

/// Misma clasificación por extensión que el gestor de escritorio (`fsys::kind_of_ext`).
pub fn kind_of_ext(ext: &str) -> Kind {
    if ext.is_empty() || ext.len() > 8 {
        return Kind::Other;
    }
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jpe" | "png" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg" | "avif" | "heic" | "heif" | "raw" | "cr2"
        | "cr3" | "nef" | "nrw" | "arw" | "srf" | "sr2" | "dng" | "orf" | "rw2" | "pef" | "raf" | "srw" | "3fr" | "erf" | "kdc" | "mrw"
        | "x3f" | "psd" | "psb" | "xcf" | "blend" | "blend1" => Kind::Image,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "ts" => Kind::Video,
        "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" | "wma" | "aiff" => Kind::Audio,
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" | "xml" | "html" | "htm" | "css" | "js" | "jsx" | "tsx" | "py" | "c" | "h"
        | "cpp" | "hpp" | "cc" | "java" | "go" | "sh" | "bat" | "ini" | "cfg" | "conf" | "log" | "csv" | "sql" | "lua" | "php" | "rb" | "cs"
        | "kt" | "swift" | "lock" | "gitignore" | "env" | "tex" | "srt" => Kind::Text,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "iso" | "deb" | "rpm" | "tgz" | "jar" => Kind::Archive,
        "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp" | "rtf" | "epub" | "cbz" | "cbr" => Kind::Doc,
        _ => Kind::Other,
    }
}
