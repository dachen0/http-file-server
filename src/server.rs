use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::request::{self, Method, Request};
use crate::response::{Response, mime_for_ext};

const CHUNK_SIZE: usize = 8 * 1024;
const MAX_RECEIVED_HEADERS_SIZE: usize = 4 * 1024;

/// Maximum number of concurrent connections. New connections are dropped when
/// this limit is reached rather than queued, preventing fd exhaustion.
const MAX_CONNECTIONS: usize = 4096;

/// Deadline for receiving complete request headers after a connection is
/// accepted (or after a keep-alive response finishes). Guards against
/// Slowloris-style attacks that hold connections open with partial headers.
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an idle keep-alive connection may wait for the next request.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(target_os = "linux")]
fn set_tcp_cork(stream: &TcpStream, enable: bool) {
    let val = enable as libc::c_int;
    let ret = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_CORK,
            std::ptr::addr_of!(val).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    debug_assert_eq!(ret, 0, "TCP_CORK setsockopt failed");
}

#[cfg(not(target_os = "linux"))]
fn set_tcp_cork(_stream: &TcpStream, _enable: bool) {}

// ---------------------------------------------------------------------------
// Sending — streams a file response after headers are dispatched
// ---------------------------------------------------------------------------

struct Sending {
    file: fs::File,
    keepalive: bool,
    #[cfg(target_os = "linux")]
    offset: libc::off_t,
}

impl Sending {
    /// Advance the file send by one chunk.
    /// Returns `Some(true)` while more data remains, `Some(false)` on clean EOF,
    /// `None` on an unrecoverable I/O error.
    #[cfg(target_os = "linux")]
    fn step(&mut self, stream: &mut TcpStream) -> Option<bool> {
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
                set_tcp_cork(stream, false);
                Some(false)
            }
            n if n > 0 => Some(true),
            _ => {
                let kind = io::Error::last_os_error().kind();
                if kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::Interrupted {
                    Some(true)
                } else {
                    None
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn step(&mut self, stream: &mut TcpStream) -> Option<bool> {
        let mut chunk = [0u8; CHUNK_SIZE];
        match self.file.read(&mut chunk) {
            Ok(0) => Some(false),
            Ok(n) => {
                if stream.write_all(&chunk[..n]).is_ok() {
                    Some(true)
                } else {
                    None
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Some(true),
            Err(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

enum AfterWrite {
    /// After flushing the header, start streaming this file.
    File(Sending),
    /// After flushing, close the connection.
    Close,
}

enum State {
    /// Performing the TLS handshake before any HTTP traffic.
    #[cfg(all(target_os = "linux", feature = "tls"))]
    TlsHandshaking { tls: Box<rustls::ServerConnection> },
    /// Accumulating incoming bytes until the full HTTP headers have arrived.
    Connecting {
        buf: [u8; MAX_RECEIVED_HEADERS_SIZE],
        offset: usize,
    },
    /// Draining `buf[written..]` to the socket non-blockingly; transitions via
    /// `after` when the buffer is fully flushed.
    Writing {
        buf: Vec<u8>,
        written: usize,
        after: AfterWrite,
    },
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
    /// Wall-clock deadline after which this connection is forcibly dropped.
    /// Reset on each state transition so the timeout tracks the *current* phase.
    deadline: Instant,
}

impl Connection {
    fn new(stream: TcpStream) -> Self {
        Connection {
            stream,
            state: State::Connecting {
                buf: [0u8; MAX_RECEIVED_HEADERS_SIZE],
                offset: 0,
            },
            deadline: Instant::now() + HEADER_TIMEOUT,
        }
    }

    /// Queue a response for non-blocking writing. For file responses, TCP_CORK
    /// is set so the header and first file chunk coalesce into fewer packets.
    fn queue_response(&mut self, response: Response) {
        match response {
            Response::File {
                header,
                file,
                keepalive,
            } => {
                set_tcp_cork(&self.stream, true);
                self.state = State::Writing {
                    buf: header,
                    written: 0,
                    after: AfterWrite::File(Sending {
                        file,
                        keepalive,
                        #[cfg(target_os = "linux")]
                        offset: 0,
                    }),
                };
            }
            Response::Bytes { header, body } => {
                let mut buf = header;
                buf.extend_from_slice(body);
                self.state = State::Writing {
                    buf,
                    written: 0,
                    after: AfterWrite::Close,
                };
            }
        }
    }

    /// Advance this connection by one step.
    /// Returns `true` to keep it, `false` to drop it.
    fn advance(&mut self, root: &Path) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        let state = std::mem::replace(&mut self.state, State::Done);

        match state {
            #[cfg(all(target_os = "linux", feature = "tls"))]
            State::TlsHandshaking { mut tls } => {
                // RX: drain socket bytes into rustls's read buffer
                loop {
                    match tls.read_tls(&mut self.stream) {
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            eprintln!("[tls] read_tls error: {e}");
                            return false;
                        }
                        Ok(0) => {
                            eprintln!("[tls] read_tls: peer closed");
                            return false;
                        }
                        Ok(_) => {}
                    }
                    if let Err(e) = tls.process_new_packets() {
                        eprintln!("[tls] process_new_packets error: {e}");
                        let _ = tls.write_tls(&mut self.stream);
                        return false;
                    }
                    break;
                }
                // TX: flush any pending outgoing handshake records
                loop {
                    match tls.write_tls(&mut self.stream) {
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Ok(0) => break,
                        Err(e) => {
                            eprintln!("[tls] write_tls error: {e}");
                            break;
                        }
                        Ok(_) => {}
                    }
                }

                if tls.is_handshaking() {
                    self.state = State::TlsHandshaking { tls };
                    return true;
                }

                // Handshake complete — extract session keys and activate kTLS.
                let version = match tls.protocol_version() {
                    Some(v) => v,
                    None => {
                        eprintln!("[tls] protocol_version() returned None");
                        return false;
                    }
                };

                // Flush any post-handshake records rustls generated (e.g.
                // NewSessionTicket in TLS 1.3).  These must reach the client
                // before we hand the fd to the kernel; once kTLS is active the
                // ServerConnection is gone and the tickets would be lost.
                loop {
                    match tls.write_tls(&mut self.stream) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }

                // Rescue any plaintext rustls already decrypted into its
                // internal buffer.  Clients (e.g. curl with TLS 1.3) often
                // pipeline the HTTP request in the same TCP write as the TLS
                // Finished message.  read_tls() consumed those bytes from the
                // kernel socket buffer into rustls; if we don't drain them
                // before dropping the ServerConnection they are lost forever.
                let mut buf = [0u8; MAX_RECEIVED_HEADERS_SIZE];
                let mut offset = 0usize;
                loop {
                    match tls.reader().read(&mut buf[offset..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => offset += n,
                    }
                }

                let tls_owned = *tls;
                let secrets = match tls_owned.dangerous_extract_secrets() {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[tls] dangerous_extract_secrets failed: {e}");
                        return false;
                    }
                };
                if let Err(e) = crate::tls::setup_ktls(self.stream.as_raw_fd(), secrets, version) {
                    eprintln!("[tls] setup_ktls failed: {e}");
                    return false;
                }
                // The kernel now handles all TLS on this fd transparently.
                self.state = State::Connecting { buf, offset };
                true
            }

            State::Connecting {
                mut buf,
                mut offset,
            } => {
                match self.stream.read(&mut buf[offset..]) {
                    Ok(0) => return false,
                    Ok(n) => offset += n,
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted =>
                    {
                        // Fall through: check whether the buffer already holds
                        // complete headers (can happen when bytes arrived via
                        // the rustls plaintext drain path after kTLS setup).
                    }
                    Err(_) => return false,
                }

                if let Some(end) = request::headers_end(&buf[..offset]) {
                    let response = match Request::parse(&buf[..end]) {
                        Ok(req) if req.method == Method::Get => {
                            serve_file(&req.path, root, req.keepalive)
                        }
                        Ok(_) => Response::method_not_allowed(),
                        Err(_) => Response::bad_request(),
                    };
                    self.queue_response(response);
                    true
                } else if offset == MAX_RECEIVED_HEADERS_SIZE {
                    // Buffer full with no header terminator — headers too large.
                    self.queue_response(Response::header_fields_too_large());
                    true
                } else {
                    self.state = State::Connecting { buf, offset };
                    true
                }
            }

            State::Writing {
                buf,
                mut written,
                after,
            } => {
                match self.stream.write(&buf[written..]) {
                    Ok(0) => return false,
                    Ok(n) => written += n,
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted =>
                    {
                        self.state = State::Writing {
                            buf,
                            written,
                            after,
                        };
                        return true;
                    }
                    Err(_) => return false,
                }
                if written < buf.len() {
                    self.state = State::Writing {
                        buf,
                        written,
                        after,
                    };
                    return true;
                }
                match after {
                    AfterWrite::File(sending) => {
                        self.state = State::Sending(sending);
                        true
                    }
                    AfterWrite::Close => false,
                }
            }

            State::Sending(mut sending) => match sending.step(&mut self.stream) {
                Some(true) => {
                    self.state = State::Sending(sending);
                    true
                }
                Some(false) if sending.keepalive => {
                    self.state = State::Connecting {
                        buf: [0u8; MAX_RECEIVED_HEADERS_SIZE],
                        offset: 0,
                    };
                    self.deadline = Instant::now() + IDLE_TIMEOUT;
                    true
                }
                _ => false,
            },

            State::Done => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// A handle that lets callers stop a running [`Server::serve`] loop.
pub struct ShutdownHandle(Arc<AtomicBool>);

impl ShutdownHandle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub struct Server {
    listener: TcpListener,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        Ok(Server {
            listener,
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Returns a handle that can stop the [`serve`](Self::serve) loop.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle(Arc::clone(&self.stop))
    }

    /// Serve static files from `root` in a single-threaded polling event loop.
    /// No threads are spawned; every connection is advanced one step per
    /// iteration of the main loop. The loop busy-spins while connections are
    /// active, trading CPU for minimal latency; it yields when idle.
    /// Call [`shutdown_handle`](Self::shutdown_handle) before `serve` to get a
    /// handle that stops the loop cleanly from another thread.
    pub fn serve<P: AsRef<Path>>(self, root: P) -> io::Result<()> {
        let root = fs::canonicalize(root.as_ref())?;
        let Server { listener, stop } = self;
        listener.set_nonblocking(true)?;

        let mut connections: Vec<Connection> = Vec::new();

        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            loop {
                if connections.len() >= MAX_CONNECTIONS {
                    break;
                }
                match listener.accept() {
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

            // No active connections — yield rather than spin-burn a CPU core.
            if connections.is_empty() {
                std::thread::yield_now();
            }
        }
    }

    /// Serve static files over HTTPS using kernel TLS (kTLS).
    ///
    /// kTLS offloads TLS encryption to the kernel after the userspace handshake,
    /// so `sendfile(2)` continues to work zero-copy for file responses.
    /// Requires Linux kernel ≥ 4.13 with `CONFIG_TLS` and the `tls` kernel module
    /// loaded (`modprobe tls`).  Only TLS 1.2 and 1.3 are accepted.
    #[cfg(all(target_os = "linux", feature = "tls"))]
    pub fn serve_tls<P: AsRef<Path>>(
        self,
        root: P,
        tls_config: Arc<rustls::ServerConfig>,
    ) -> io::Result<()> {
        let root = fs::canonicalize(root.as_ref())?;
        let Server { listener, stop } = self;
        listener.set_nonblocking(true)?;

        let mut connections: Vec<Connection> = Vec::new();

        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            loop {
                if connections.len() >= MAX_CONNECTIONS {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        if stream.set_nonblocking(true).is_ok() {
                            match rustls::ServerConnection::new(Arc::clone(&tls_config)) {
                                Ok(tls) => connections.push(Connection {
                                    stream,
                                    state: State::TlsHandshaking { tls: Box::new(tls) },
                                    deadline: Instant::now() + HEADER_TIMEOUT,
                                }),
                                Err(e) => eprintln!("[https-server] TLS init error: {e}"),
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => eprintln!("[https-server] accept error: {e}"),
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

fn serve_file(url_path: &str, root: &Path, keepalive: bool) -> Response {
    // Reject any path that contains ".." segments before touching the filesystem.
    // This ensures traversal attempts always get 403, regardless of whether the
    // escaped path happens to exist. The canonicalize+starts_with check below
    // still provides defense-in-depth against symlink escapes.
    if url_path.split('/').any(|seg| seg == "..") {
        return Response::forbidden();
    }

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
            Response::ok(file, mime_for_ext(ext), keepalive)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Response::not_found(),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Response::forbidden(),
        Err(_) => Response::internal_error(),
    }
}
