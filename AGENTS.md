# AI Summary: `http-file-server`

## What this project is

`http-file-server` is a minimal Rust static file server built directly on `std::net::TcpListener` and nonblocking `TcpStream`s. It serves files from a filesystem root over HTTP/1.1 or HTTPS, and keeps the implementation compact and low-level.

The codebase favors:

- minimal dependencies (`libc`, `rustls`, `rustls-pemfile`)
- explicit socket and filesystem control via direct syscalls
- a single-threaded nonblocking polling event loop
- simple request parsing limited to `GET` requests
- streaming file responses without buffering entire files in memory
- kernel TLS offload (kTLS) so encryption stays in the kernel hot path

## High-level architecture

The crate is split into four internal modules and one public entry point:

- `src/lib.rs` — exposes `Server`, `ShutdownHandle`, and `tls` module; contains integration tests
- `src/server.rs` — event loop, connection state machine, file resolution
- `src/request.rs` — parses the request line, decodes percent-encoded paths
- `src/response.rs` — builds serialized HTTP responses, maps extensions to MIME types
- `src/tls.rs` — kTLS kernel ABI structs, `setup_ktls()`, `load_server_config()`
- `examples/serve.rs` — runnable example; switches to HTTPS when `TLS_CERT`/`TLS_KEY` are set

## Connection state machine

Every accepted socket is represented as a `Connection` with a `State` enum. The event loop calls `connection.advance(&root)` once per iteration; `advance` returns `false` when the connection should be dropped.

States in order:

1. **`TlsHandshaking`** *(Linux + `tls` feature only)* — non-blocking rustls handshake. Each call to `advance` drains bytes from the socket into rustls (`read_tls`), processes records (`process_new_packets`), and flushes outgoing handshake records (`write_tls`). When `is_handshaking()` returns false, session keys are extracted with `dangerous_extract_secrets()` and handed to the kernel via `setup_ktls()`. The state then transitions directly to `Connecting`.

2. **`Connecting`** — reads request bytes into a 4 KiB stack buffer until `\r\n\r\n` is found. Parses the request line and builds a `Response`. If the buffer fills without finding the terminator, responds with `431`.

3. **`Writing`** — drains the response header `Vec<u8>` to the socket non-blockingly. On completion transitions to `Sending` (file response) or closes the connection (error response).

4. **`Sending`** — on Linux: calls `libc::sendfile()` in 8 KiB chunks. On other platforms: read-then-write with a 8 KiB stack buffer. When EOF is reached, TCP_CORK is lifted.

5. **`Done`** — placeholder used only during `std::mem::replace` to move state out of `&mut self`.

## kTLS design

kTLS offloads TLS record encryption/decryption to the Linux kernel. The sequence is:

1. `rustls::ServerConnection` performs the handshake over the TCP socket in non-blocking mode.
2. Once `is_handshaking()` is false, `dangerous_extract_secrets()` returns `ExtractedSecrets { tx: (seq, secrets), rx: (seq, secrets) }`.
3. `setup_ktls(fd, secrets, version)`:
   - calls `setsockopt(IPPROTO_TCP, TCP_ULP, "tls")` to attach the TLS ULP to the socket
   - builds a `#[repr(C)]` struct matching the kernel's `tls12_crypto_info_aes_gcm_128`, `_aes_gcm_256`, or `_chacha20_poly1305` layout
   - calls `setsockopt(SOL_TLS, TLS_TX, &info)` and `setsockopt(SOL_TLS, TLS_RX, &info)`
4. After that, all I/O on the fd is transparently encrypted by the kernel — including `sendfile(2)`.

For AES-GCM, the 12-byte rustls `Iv` is split as `salt = iv[0..4]`, `iv = iv[4..12]`. The `rec_seq` field is the 8-byte big-endian sequence number from `ExtractedSecrets`. ChaCha20-Poly1305 uses the full 12 bytes as the IV with no salt field.

Cipher structs have compile-time size assertions to catch ABI drift: AES-128-GCM → 40 bytes, AES-256-GCM → 56 bytes, ChaCha20-Poly1305 → 56 bytes.

## HTTP behavior

- `GET /path` — resolves against root, serves the file
- `GET /dir/` — serves `dir/index.html`
- non-`GET` — `405 Method Not Allowed`
- missing path — `404 Not Found`
- path escaping root — `403 Forbidden` (checked via `fs::canonicalize` + `starts_with(root)`)
- oversized headers — `431 Request Header Fields Too Large`
- connection is always closed after the response (no keep-alive)

## Linux-specific optimizations

- `TCP_CORK`: enabled before sending response headers so headers and the first file chunk coalesce; disabled at EOF to flush the last segment
- `sendfile(2)`: streams file content kernel-to-kernel with no userspace copy; works transparently through kTLS when TLS offload is active

Non-Linux builds fall back to a read-then-write loop; `TCP_CORK` and kTLS are compiled out.

## Feature flags

- `tls` (default) — pulls in `rustls` and `rustls-pemfile`, enables `State::TlsHandshaking`, `Server::serve_tls()`, and `crate::tls`
- disabling `tls` (`--no-default-features`) produces a binary with no TLS code and only the `libc` dependency

## Security notes

- Path traversal: segments equal to `..` are rejected before any filesystem access; `canonicalize` + `starts_with(root)` provides defense-in-depth against symlink escapes
- TLS 1.2 and 1.3 only — rustls does not implement TLS 1.0 or 1.1; the `tls12` feature is explicitly included to support both
- `enable_secret_extraction = true` must be set on `ServerConfig` for `dangerous_extract_secrets()` to succeed

## Testing

Tests in `src/lib.rs` spin up a real server on `127.0.0.1:0` and send raw TCP requests. Coverage includes:

- file serving (200)
- 404 for missing files
- directory → `index.html`
- path traversal → 403
- non-GET → 405
- content-type header
- percent-decoded paths
- request parsing helpers
- MIME fallback
