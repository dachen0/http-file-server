/// Returns the byte offset just past `\r\n\r\n`, or `None` if headers are incomplete.
pub fn headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

#[derive(Debug, PartialEq)]
pub enum Method {
    Get,
    Other(String),
}

#[derive(Debug)]
pub struct Request {
    pub method: Method,
    pub path: String,
}

#[derive(Debug)]
pub enum ParseError {
    InvalidRequestLine,
}

impl Request {
    /// Parse an HTTP request from a byte slice that contains the complete headers.
    pub fn parse(buf: &[u8]) -> Result<Self, ParseError> {
        let line_end = buf
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or(ParseError::InvalidRequestLine)?;

        let line =
            std::str::from_utf8(&buf[..line_end]).map_err(|_| ParseError::InvalidRequestLine)?;

        let mut parts = line.splitn(3, ' ');
        let method_str = parts.next().ok_or(ParseError::InvalidRequestLine)?;
        let raw_path = parts.next().ok_or(ParseError::InvalidRequestLine)?;
        let version = parts.next().ok_or(ParseError::InvalidRequestLine)?;
        if version != "HTTP/1.1" && version != "HTTP/1.0" {
            return Err(ParseError::InvalidRequestLine);
        }

        let method = match method_str {
            "GET" => Method::Get,
            other => Method::Other(other.to_string()),
        };

        let path_only = raw_path.split('?').next().unwrap_or("/");
        let path = percent_decode(path_only).map_err(|_| ParseError::InvalidRequestLine)?;

        Ok(Request { method, path })
    }
}

fn percent_decode(s: &str) -> Result<String, std::string::FromUtf8Error> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_end_found() {
        let buf = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(headers_end(buf), Some(buf.len()));
    }

    #[test]
    fn headers_end_incomplete() {
        assert_eq!(headers_end(b"GET / HTTP/1.1\r\n"), None);
    }

    #[test]
    fn parse_get() {
        let buf = b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n";
        let req = Request::parse(buf).unwrap();
        assert_eq!(req.method, Method::Get);
        assert_eq!(req.path, "/hello");
    }

    #[test]
    fn parse_strips_query_and_decodes() {
        let buf = b"GET /hello%20world?v=1 HTTP/1.1\r\n\r\n";
        let req = Request::parse(buf).unwrap();
        assert_eq!(req.path, "/hello world");
    }
}
