# `http-file-server`

A compact static file server in Rust with a single-threaded nonblocking event loop and Linux-optimized file streaming.

## Features

- serves files from a chosen directory over HTTP
- supports `GET` requests
- serves `index.html` for directory paths
- blocks path traversal outside the configured root
- decodes percent-encoded paths like `%20`
- streams files instead of loading entire bodies into memory
- uses `sendfile` and `TCP_CORK` on Linux

## Project layout

- `src/lib.rs` — public crate entry point and end-to-end tests
- `src/server.rs` — event loop, connection state, and file serving
- `src/request.rs` — request-line parsing and path decoding
- `src/response.rs` — HTTP response serialization and MIME mapping
- `examples/serve.rs` — runnable example server

## Run the example server

```bash
cargo run --example serve -- /path/to/site
```

If no directory is provided, the example serves the current working directory.

Default bind address:

```text
127.0.0.1:8080
```

Then open:

```text
http://127.0.0.1:8080
```

## Use as a library

```rust
use http_file_server::Server;

fn main() -> std::io::Result<()> {
    let server = Server::bind("127.0.0.1:8080")?;
    server.serve("./public")
}
```

## Behavior

- `GET /file.txt` returns the file if it exists
- `GET /docs/` serves `docs/index.html` when present
- non-`GET` methods return `405 Method Not Allowed`
- missing files return `404 Not Found`
- escaped paths outside the root return `403 Forbidden`
- error responses are plain text and connections are closed after each response

## Development

Run tests:

```bash
cargo test
```

## Notes and limitations

- no async runtime and no thread pool
- no keep-alive connections
- no range requests
- no directory listing generation
- no TLS support
- intended as a small, readable server rather than a full production web stack

## Summary

If you want a small Rust codebase that demonstrates manual HTTP parsing, nonblocking socket handling, safe static file resolution, and efficient Linux file streaming, this project is a good fit.
