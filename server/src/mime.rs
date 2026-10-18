//! Tipos MIME de una lista cerrada. Solo lo que es inofensivo se muestra «inline»;
//! todo lo demás (HTML, SVG, scripts…) se descarga siempre como octet-stream.

pub struct Kind {
    pub mime: &'static str,
    pub inline: bool,
}

pub fn of(name: &str) -> Kind {
    let ext = match name.rsplit_once('.') {
        Some((_, e)) => e.to_ascii_lowercase(),
        None => String::new(),
    };
    let (mime, inline) = match ext.as_str() {
        "png" => ("image/png", true),
        "jpg" | "jpeg" => ("image/jpeg", true),
        "gif" => ("image/gif", true),
        "webp" => ("image/webp", true),
        "bmp" => ("image/bmp", true),
        "ico" => ("image/x-icon", true),
        "mp4" | "m4v" => ("video/mp4", true),
        "webm" => ("video/webm", true),
        "mp3" => ("audio/mpeg", true),
        "ogg" | "oga" | "opus" => ("audio/ogg", true),
        "flac" => ("audio/flac", true),
        "wav" => ("audio/wav", true),
        "m4a" => ("audio/mp4", true),
        "pdf" => ("application/pdf", true),
        "txt" | "md" | "log" | "csv" | "json" | "toml" | "yaml" | "yml" | "rs" | "c" | "h" | "cpp" | "py" | "sh" | "ini"
        | "cfg" | "conf" | "xml" | "js" | "css" | "ts" | "go" | "java" | "tex" | "srt" | "diff" | "patch" => {
            ("text/plain; charset=utf-8", true)
        }
        "zip" => ("application/zip", false),
        _ => ("application/octet-stream", false),
    };
    Kind { mime, inline }
}
