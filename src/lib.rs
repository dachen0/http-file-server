mod request;
mod response;
mod server;

#[cfg(all(target_os = "linux", feature = "tls"))]
pub mod tls;

pub use server::{Server, ShutdownHandle};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_server() -> (std::net::SocketAddr, TempDir) {
        let root = std::env::temp_dir().join(format!(
            "http_test_{}",
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();

        let server = Server::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let root_clone = root.clone();
        std::thread::spawn(move || {
            let _ = server.serve(root_clone);
        });

        (addr, TempDir(root))
    }

    fn get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
        let mut conn = TcpStream::connect(addr).unwrap();
        write!(conn, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut buf = String::new();
        conn.read_to_string(&mut buf).unwrap();
        let status: u16 = buf
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        (status, buf)
    }

    #[test]
    fn serves_existing_file() {
        let (addr, dir) = make_server();
        fs::write(dir.0.join("hello.txt"), b"hello world").unwrap();

        let (status, body) = get(addr, "/hello.txt");
        assert_eq!(status, 200);
        assert!(body.ends_with("hello world"));
    }

    #[test]
    fn returns_404_for_missing_file() {
        let (addr, _dir) = make_server();
        let (status, _) = get(addr, "/does_not_exist.txt");
        assert_eq!(status, 404);
    }

    #[test]
    fn serves_index_html_for_directory() {
        let (addr, dir) = make_server();
        fs::create_dir_all(dir.0.join("sub")).unwrap();
        fs::write(dir.0.join("sub/index.html"), b"<h1>Index</h1>").unwrap();

        let (status, body) = get(addr, "/sub/");
        assert_eq!(status, 200);
        assert!(body.contains("<h1>Index</h1>"));
    }

    #[test]
    fn rejects_path_traversal() {
        let (addr, _dir) = make_server();
        let (status, _) = get(addr, "/../etc/passwd");
        // 403 (traversal detected); never 200.
        assert!(status == 403, "unexpected status {status}");
    }

    #[test]
    fn returns_405_for_non_get() {
        let (addr, dir) = make_server();
        fs::write(dir.0.join("f.txt"), b"x").unwrap();

        let mut conn = TcpStream::connect(addr).unwrap();
        write!(conn, "POST /f.txt HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut buf = String::new();
        conn.read_to_string(&mut buf).unwrap();
        let status: u16 = buf
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        assert_eq!(status, 405);
    }

    #[test]
    fn correct_content_type_for_html() {
        let (addr, dir) = make_server();
        fs::write(dir.0.join("page.html"), b"<p>hi</p>").unwrap();

        let (_status, response) = get(addr, "/page.html");
        assert!(response.contains("text/html"));
    }

    #[test]
    fn percent_encoded_path_decoded() {
        let (addr, dir) = make_server();
        fs::write(dir.0.join("hello world.txt"), b"spaces").unwrap();

        let (status, body) = get(addr, "/hello%20world.txt");
        assert_eq!(status, 200);
        assert!(body.ends_with("spaces"));
    }
}
