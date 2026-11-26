//! Which uploads a shop may store, recognized by their first bytes rather
//! than by the name or content type the client sends.

/// A file type SOAT accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaType {
    pub mime: &'static str,
    pub extension: &'static str,
}

impl MediaType {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
}

const PNG: MediaType = MediaType {
    mime: "image/png",
    extension: "png",
};
const JPEG: MediaType = MediaType {
    mime: "image/jpeg",
    extension: "jpg",
};
const GIF: MediaType = MediaType {
    mime: "image/gif",
    extension: "gif",
};
const WEBP: MediaType = MediaType {
    mime: "image/webp",
    extension: "webp",
};
const AVIF: MediaType = MediaType {
    mime: "image/avif",
    extension: "avif",
};
const PDF: MediaType = MediaType {
    mime: "application/pdf",
    extension: "pdf",
};
const MP4: MediaType = MediaType {
    mime: "video/mp4",
    extension: "mp4",
};
const WEBM: MediaType = MediaType {
    mime: "video/webm",
    extension: "webm",
};
const ZIP: MediaType = MediaType {
    mime: "application/zip",
    extension: "zip",
};
const EPUB: MediaType = MediaType {
    mime: "application/epub+zip",
    extension: "epub",
};
const MP3: MediaType = MediaType {
    mime: "audio/mpeg",
    extension: "mp3",
};

/// The type of a file from its first bytes, or `None` when it is not one
/// SOAT stores (HTML and SVG are refused on purpose: they can carry
/// scripts).
pub fn detect(bytes: &[u8]) -> Option<MediaType> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some(PNG)
    } else if starts(b"\xff\xd8\xff") {
        Some(JPEG)
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some(GIF)
    } else if starts(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(WEBP)
    } else if starts(b"%PDF-") {
        Some(PDF)
    } else if starts(b"\x1a\x45\xdf\xa3") {
        Some(WEBM)
    } else if starts(b"PK\x03\x04") {
        // An EPUB is a ZIP whose first entry names its type.
        if bytes.get(30..58) == Some(b"mimetypeapplication/epub+zip") {
            Some(EPUB)
        } else {
            Some(ZIP)
        }
    } else if starts(b"ID3") || starts(b"\xff\xfb") || starts(b"\xff\xf3") {
        Some(MP3)
    } else if bytes.get(4..8) == Some(b"ftyp") {
        match bytes.get(8..12) {
            Some(b"avif" | b"avis") => Some(AVIF),
            Some(b"isom" | b"iso2" | b"mp41" | b"mp42" | b"avc1" | b"M4V ") => Some(MP4),
            _ => None,
        }
    } else {
        None
    }
}

/// A file name safe to put in a storage key and a download: the stem
/// reduced to `a-z0-9-_`, at most 80 characters, with the detected
/// extension (`My Photo (1).PNG` with PNG bytes becomes `my-photo-1.png`).
pub fn safe_file_name(name: &str, kind: MediaType) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    let mut out = String::new();
    for c in stem.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 80 {
            break;
        }
    }
    let stem = out.trim_end_matches('-');
    let stem = if stem.is_empty() { "file" } else { stem };
    format!("{stem}.{}", kind.extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_what_it_stores() {
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n...."), Some(PNG));
        assert_eq!(detect(b"\xff\xd8\xff\xe0"), Some(JPEG));
        assert_eq!(detect(b"RIFF\0\0\0\0WEBPVP8 "), Some(WEBP));
        assert_eq!(detect(b"\0\0\0\x1cftypavif"), Some(AVIF));
        assert_eq!(detect(b"\0\0\0\x18ftypisom"), Some(MP4));
        assert_eq!(detect(b"%PDF-1.7"), Some(PDF));
        assert_eq!(detect(b"PK\x03\x04\x14\0"), Some(ZIP));
        let mut epub = b"PK\x03\x04".to_vec();
        epub.resize(30, 0);
        epub.extend_from_slice(b"mimetypeapplication/epub+zip");
        assert_eq!(detect(&epub), Some(EPUB));
        assert_eq!(detect(b"ID3\x04\0"), Some(MP3));
        assert!(detect(b"\x89PNG").is_none());
    }

    #[test]
    fn refuses_markup_and_unknown_bytes() {
        assert_eq!(detect(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"), None);
        assert_eq!(detect(b"<!doctype html><script>"), None);
        assert_eq!(detect(b"name,price\n"), None);
        assert_eq!(detect(b""), None);
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("My Photo (1).PNG", PNG), "my-photo-1.png");
        assert_eq!(safe_file_name("../../etc/passwd", PNG), "passwd.png");
        assert_eq!(safe_file_name("C:\\x\\a b.jpg", JPEG), "a-b.jpg");
        assert_eq!(safe_file_name("évier.svg", PNG), "vier.png");
        assert_eq!(safe_file_name("", PDF), "file.pdf");
    }
}
