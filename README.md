# `http-file-server`

A compact static file server in Rust with a single-threaded nonblocking event loop, Linux-optimized file streaming, and HTTPS via kernel TLS offload (kTLS).

## Features

- serves static files over HTTP or HTTPS
- HTTPS uses kTLS — TLS encryption is handled by the Linux kernel, so `sendfile` remains zero-copy
- supports TLS 1.2 and TLS 1.3 only
- supports keep-alive
- single-threaded nonblocking event loop, no async runtime
- `sendfile(2)` and `TCP_CORK` on Linux for efficient file delivery
- decodes percent-encoded paths
- blocks path traversal outside the configured root
- serves `index.html` for directory paths

## Project layout

- `src/lib.rs` — public crate entry point and integration tests
- `src/server.rs` — event loop, connection state machine, file serving
- `src/request.rs` — request-line parsing and path decoding
- `src/response.rs` — HTTP response serialization and MIME mapping
- `src/tls.rs` — kTLS setsockopt structs, kernel TLS activation, certificate loading
- `examples/serve.rs` — runnable example server

## Run the example server

### HTTP

```bash
cargo run --example serve -- /path/to/site
```

Default bind address: `127.0.0.1:8080`. Override with the `BIND` environment variable.

### HTTPS (kTLS)

Generate a certificate first:

```bash
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem \
  -days 365 -nodes -subj '/CN=localhost' \
  -addext 'subjectAltName=IP:127.0.0.1'
```

Then run:

```bash
TLS_CERT=cert.pem TLS_KEY=key.pem cargo run --example serve -- /path/to/site
```

Requires the `tls` kernel module to be loaded:

```bash
sudo modprobe tls
```

## Use as a library

### HTTP

```rust
use http_file_server::Server;

fn main() -> std::io::Result<()> {
    let server = Server::bind("127.0.0.1:8080")?;
    server.serve("./public")
}
```

### HTTPS

```rust
use std::sync::Arc;
use http_file_server::{Server, tls};

fn main() -> std::io::Result<()> {
    let config = tls::load_server_config(
        std::path::Path::new("cert.pem"),
        std::path::Path::new("key.pem"),
    )?;
    let server = Server::bind("0.0.0.0:443")?;
    server.serve_tls("./public", config)
}
```

## HTTP behavior

- `GET /file.txt` — returns the file if it exists
- `GET /docs/` — serves `docs/index.html` when present
- non-`GET` methods — `405 Method Not Allowed`
- missing files — `404 Not Found`
- paths escaping the root — `403 Forbidden`
- oversized request headers — `431 Request Header Fields Too Large`
- connections close after each response (no keep-alive)

## How kTLS works

After the TLS handshake completes in userspace (via rustls), the session keys are handed to the kernel with `setsockopt(TCP_ULP, "tls")` plus `setsockopt(SOL_TLS, TLS_TX/RX, ...)`. From that point, all reads and writes on the socket — including `sendfile(2)` — are transparently encrypted and decrypted by the kernel. The application-layer code in `Connecting`, `Writing`, and `Sending` states is identical for HTTP and HTTPS.

## Development

```bash
cargo test
cargo build
```

## Notes and limitations

- Linux only for kTLS, `sendfile`, and `TCP_CORK`; plain HTTP builds and runs on other platforms
- no range requests
- no directory listing
- no client certificate authentication
