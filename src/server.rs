use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::request::{self, Method, Request};
use crate::response::{mime_for_ext, Response};

const CHUNK_SIZE: usize = 8 * 1024;
const MAX_RECEIVED_HEADERS_SIZE: usize = 4 * 1024;

#[cfg(target_os = "linux")]
fn set_tcp_cork(stream: &TcpStream, enable: bool) {
    let val = enable as libc::c_int;
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_CORK,
            std::ptr::addr_of!(val).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn set_tcp_cork(_stream: &TcpStream, _enable: bool) {}

// ---------------------------------------------------------------------------
// Sending — streams a file response after headers are dispatched
// ---------------------------------------------------------------------------

struct Sending {
    file: fs::File,
    #[cfg(target_os = "linux")]
    offset: libc::off_t,
}

impl Sending {
    #[cfg(target_os = "linux")]
    fn step(&mut self, stream: &mut TcpStream) -> bool {
        let ret = unsafe {
            libc::sendfile(
                stream.as_raw_fd(),
                self.file.as_raw_fd(),
                &mut self.offset,
                CHUNK_SIZE,
            )
        };
        match ret {
            0 => {
                // EOF — lift the cork so the kernel flushes any buffered data.
                set_tcp_cork(stream, false);
                false
            }
            n if n > 0 => true,
            _ => io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock,
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn step(&mut self, stream: &mut TcpStream) -> bool {
        let mut chunk = [0u8; CHUNK_SIZE];
        match self.file.read(&mut chunk) {
            Ok(0) => false,
            Ok(n) => stream.write_all(&chunk[..n]).is_ok(),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => true,
            Err(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

enum State {
    /// Accumulating incoming bytes until the full HTTP headers have arrived.
    Connecting { buf: [u8; MAX_RECEIVED_HEADERS_SIZE], offset: usize },
    /// Headers parsed; streaming the file response.
    Sending(Sending),
    /// Placeholder used only during `std::mem::replace`.
    Done,
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

struct Connection {
    stream: TcpStream,
    state: State,
}

impl Connection {
    fn new(stream: TcpStream) -> Self {
        Connection { stream, state: State::Connecting { buf: [0u8; MAX_RECEIVED_HEADERS_SIZE], offset: 0 } }
    }

    /// Advance this connection by one step.
    /// Returns `true` to keep it, `false` to drop it.
    fn advance(&mut self, root: &Path) -> bool {
        let state = std::mem::replace(&mut self.state, State::Done);

        match state {
            State::Connecting { mut buf, mut offset } => {
                match self.stream.read(&mut buf[offset..]) {
                    Ok(0) => return false,
                    Ok(n) => offset += n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        self.state = State::Connecting { buf, offset };
                        return true;
                    }
                    Err(_) => return false,
                }

                if let Some(end) = request::headers_end(&buf) {
                    let response = match Request::parse(&buf[..end]) {
                        Ok(req) if req.method == Method::Get => serve_file(&req.path, root),
                        Ok(_) => Response::method_not_allowed(),
                        Err(_) => Response::bad_request(),
                    };
                    match response {
                        Response::File { header, file } => {
                            // Cork so the header and the first file chunk
                            // are coalesced into as few packets as possible.
                            set_tcp_cork(&self.stream, true);
                            let _ = self.stream.write_all(&header);
                            self.state = State::Sending(Sending {
                                file,
                                #[cfg(target_os = "linux")]
                                offset: 0,
                            });
                            true
                        }
                        Response::Bytes { header, body } => {
                            // Error responses are small; write them directly and close.
                            let _ = self.stream.write_all(&header);
                            let _ = self.stream.write_all(body);
                            false
                        }
                    }
                } else {
                    self.state = State::Connecting { buf, offset };
                    true
                }
            }

            State::Sending(mut sending) => {
                let keep = sending.step(&mut self.stream);
                if keep {
                    self.state = State::Sending(sending);
                }
                keep
            }

            State::Done => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub struct Server {
    listener: TcpListener,
}

impl Server {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        Ok(Server { listener })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve static files from `root` in a single-threaded event loop.
    /// No threads are spawned; every connection is advanced one step per
    /// iteration of the main loop.
    pub fn serve<P: AsRef<Path>>(self, root: P) -> io::Result<()> {
        let root = fs::canonicalize(root.as_ref())?;
        self.listener.set_nonblocking(true)?;

        let mut connections: Vec<Connection> = Vec::new();

        loop {
            loop {
                match self.listener.accept() {
                    Ok((stream, _addr)) => {
                        if stream.set_nonblocking(true).is_ok() {
                            connections.push(Connection::new(stream));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => eprintln!("[http-server] accept error: {e}"),
                }
            }

            connections.retain_mut(|conn| conn.advance(&root));

            if connections.is_empty() {
                std::thread::yield_now();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// File resolution
// ---------------------------------------------------------------------------

fn serve_file(url_path: &str, root: &Path) -> Response {
    let candidate = root.join(url_path.trim_start_matches('/'));

    let canonical = match fs::canonicalize(&candidate) {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Response::not_found(),
        Err(_) => return Response::internal_error(),
    };

    if !canonical.starts_with(root) {
        return Response::forbidden();
    }

    let file_path: PathBuf = if canonical.is_dir() {
        canonical.join("index.html")
    } else {
        canonical
    };

    match fs::File::open(&file_path) {
        Ok(file) => {
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            Response::ok(file, mime_for_ext(ext))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Response::not_found(),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Response::forbidden(),
        Err(_) => Response::internal_error(),
    }
}
