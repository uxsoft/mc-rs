//! Nerd Fonts glyphs, chosen from the upstream glyphnames.json catalog:
//! https://github.com/ryanoasis/nerd-fonts/blob/master/glyphnames.json
//! Classification uses listing metadata only, including for archive/VFS entries.

pub const PARENT: &str = "\u{f062}"; // fa-arrow_up

pub fn for_entry(name: &str, directory: bool, link: bool) -> &'static str {
    if link {
        return "\u{f0c1}"; // fa-link (also for directory and broken links)
    }
    if directory {
        return "\u{f07b}"; // fa-folder
    }
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "cargo.toml" | "cargo.lock" => return "\u{e7a8}", // dev-rust
        "dockerfile"
        | "compose.yaml"
        | "compose.yml"
        | "docker-compose.yml"
        | "docker-compose.yaml" => return "\u{e7b0}", // dev-docker
        ".gitignore" | ".gitattributes" | ".gitmodules" => return "\u{e702}", // dev-git
        "makefile" | "cmakelists.txt" | "justfile" => return "\u{f085}", // fa-gears
        "license" | "licence" | "copying" | "license.md" | "license.txt" => {
            return "\u{f0e3}"; // fa-gavel
        }
        ".env" => return "\u{f013}", // fa-gear
        _ => {}
    }
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "zip" | "rar" | "tar" | "7z" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz" | "lzma"
        | "tbz2" | "txz" => "\u{f1c6}", // fa-file_zipper
        "rs" => "\u{e7a8}",                         // dev-rust
        "py" | "pyi" | "pyw" => "\u{e73c}",         // dev-python
        "js" | "jsx" | "mjs" | "cjs" => "\u{e74e}", // dev-javascript
        "html" | "htm" => "\u{e736}",               // dev-html5
        "css" | "scss" | "sass" => "\u{e749}",      // dev-css3
        "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "go" | "java" | "kt" | "swift" | "rb" | "php"
        | "lua" | "ts" | "tsx" | "vue" | "svelte" => "\u{f1c9}", // fa-file_code
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => "\u{f120}", // fa-terminal
        "json" | "jsonc" | "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "xml" | "env" => {
            "\u{f013}"
        } // fa-gear
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff"
        | "avif" | "heic" => "\u{f1c5}", // fa-file_image
        "mp3" | "wav" | "flac" | "ogg" | "opus" | "m4a" | "aac" | "aiff" => "\u{f1c7}", // fa-file_audio
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "mpeg" | "mpg" | "m4v" => "\u{f1c8}", // fa-file_video
        "pdf" => "\u{f1c1}",                                  // fa-file_pdf
        "doc" | "docx" | "odt" | "rtf" => "\u{f1c2}",         // fa-file_word
        "xls" | "xlsx" | "ods" | "csv" | "tsv" => "\u{f1c3}", // fa-file_excel
        "ppt" | "pptx" | "odp" => "\u{f1c4}",                 // fa-file_powerpoint
        "db" | "sqlite" | "sqlite3" | "sql" => "\u{f1c0}",    // fa-database
        "ttf" | "otf" | "woff" | "woff2" => "\u{f031}",       // fa-font
        "exe" | "msi" | "dll" | "so" | "dylib" | "bin" => "\u{f085}", // fa-gears
        "md" | "txt" | "rst" | "log" | "adoc" => "\u{f15c}",  // fa-file_lines
        _ => "\u{f15b}", // fa-file: unknown types and extensionless files
    }
}
