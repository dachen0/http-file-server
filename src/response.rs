use crate::VERSION;
use std::fs;

/// A response that carries its own serialised header bytes plus a body.
/// `File` responses are streamed in chunks; `Bytes` responses are written
/// directly to the stream in one shot (they are always small error messages).
pub enum Response {
    File {
        header: Vec<u8>,
        file: fs::File,
    },
    Bytes {
        header: Vec<u8>,
        body: &'static [u8],
    },
}

impl Response {
    pub fn ok(file: fs::File, content_type: &'static str) -> Self {
        let size = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => return Self::internal_error(),
        };
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {size}\r\nServer: fast-http/{VERSION}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        Response::File { header, file }
    }

    pub fn not_found() -> Self {
        Self::error(404, "Not Found", b"404 Not Found")
    }

    pub fn forbidden() -> Self {
        Self::error(403, "Forbidden", b"403 Forbidden")
    }

    pub fn method_not_allowed() -> Self {
        Self::error(405, "Method Not Allowed", b"405 Method Not Allowed")
    }

    pub fn bad_request() -> Self {
        Self::error(400, "Bad Request", b"400 Bad Request")
    }

    pub fn header_fields_too_large() -> Self {
        Self::error(
            431,
            "Request Header Fields Too Large",
            b"431 Request Header Fields Too Large",
        )
    }

    pub fn internal_error() -> Self {
        Self::error(500, "Internal Server Error", b"500 Internal Server Error")
    }

    fn error(status: u16, reason: &'static str, body: &'static [u8]) -> Self {
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        Response::Bytes { header, body }
    }
}

pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_html() {
        assert_eq!(mime_for_ext("html"), "text/html; charset=utf-8");
        assert_eq!(mime_for_ext("HTML"), "text/html; charset=utf-8");
    }

    #[test]
    fn mime_unknown_falls_back() {
        assert_eq!(mime_for_ext("xyz"), "application/octet-stream");
    }

    #[test]
    fn error_response_starts_with_status() {
        if let Response::Bytes { header, .. } = Response::not_found() {
            assert!(header.starts_with(b"HTTP/1.1 404"));
        } else {
            panic!("expected Bytes variant");
        }
    }

    #[test]
    fn too_large_headers_uses_431() {
        if let Response::Bytes { header, .. } = Response::header_fields_too_large() {
            assert!(header.starts_with(b"HTTP/1.1 431"));
        } else {
            panic!("expected Bytes variant");
        }
    }
}
