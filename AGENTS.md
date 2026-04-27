# AI Summary: `http-file-server`

## What this project is

`http-file-server` is a small Rust static file server built directly on `std::net::TcpListener` and nonblocking `TcpStream`s. It serves files from a filesystem root over HTTP/1.1 and keeps the implementation intentionally compact and low-level.

The codebase favors:

- minimal dependencies (`libc` only)
- explicit socket and filesystem control
- a single-threaded event loop
- simple request parsing for `GET` requests
- streaming file responses instead of buffering full files in memory

## High-level architecture

The crate is split into three internal modules and one public entry point:

- `src/lib.rs` exposes `Server` and contains integration-style tests
- `src/server.rs` owns the event loop, connection state machine, and file resolution
- `src/request.rs` parses the request line and decodes percent-encoded paths
- `src/response.rs` builds serialized HTTP responses and MIME type headers
- `examples/serve.rs` shows the intended executable usage

## Request lifecycle

1. `Server::bind` creates a `TcpListener`.
2. `Server::serve` canonicalizes the document root, switches the listener to nonblocking mode, and enters an infinite loop.
3. The loop accepts as many ready connections as possible and stores each one as a `Connection`.
4. Each `Connection` advances through a small state machine:
   - `Connecting`: read request bytes until `\r\n\r\n`
   - parse request
   - resolve file path and build a `Response`
   - `Sending`: stream file contents if the response is file-backed
5. Finished or failed connections are dropped from the in-memory connection list.

## Connection model

This is a cooperative, single-threaded server:

- no worker pool
- no async runtime
- no per-connection thread spawning
- each active connection gets one advancement step per loop iteration

That keeps the implementation easy to follow, but it also means scalability depends on this manual polling loop and the OS socket readiness behavior.

## HTTP behavior

Supported behavior visible from the code and tests:

- supports `GET`
- returns `405 Method Not Allowed` for non-`GET` methods
- strips query strings before filesystem lookup
- percent-decodes request paths
- serves `index.html` when the resolved path is a directory
- returns `404` for missing files
- returns `403` for path traversal outside the root
- returns `500` on parse or unexpected internal errors
- always closes the connection after the response

Not implemented:

- keep-alive / persistent connections
- partial content / range requests
- directory listings
- MIME sniffing
- HTTP version negotiation
- request bodies
- routing beyond direct file lookup

## Security and safety notes

The main filesystem protection is in `serve_file`:

- candidate paths are joined against the configured root
- `fs::canonicalize` resolves symlinks and normalizes the path
- the resolved path must still start with the canonicalized root

This is the key defense against `..` traversal and symlink escape.

## Performance notes

The server includes a Linux-specific optimization:

- `TCP_CORK` is enabled while sending headers plus the first file bytes
- `libc::sendfile` is used on Linux to stream file content kernel-to-kernel

On non-Linux targets:

- file data is copied through a fixed 8 KiB userspace buffer

This makes Linux the fast path while preserving portability elsewhere.

## Testing coverage

The current tests cover the core behavior well for a compact codebase:

- successful file serving
- `404` for missing files
- directory `index.html` serving
- path traversal rejection
- `405` for non-`GET`
- HTML content type
- percent-decoded paths
- request parsing helpers
- MIME fallback behavior

Tests are lightweight and spin up a real server bound to `127.0.0.1:0`, which gives good confidence in the end-to-end flow.

## Best way to describe this project

This is best described as:

> a compact educational static file server in Rust with a manual nonblocking event loop and Linux-optimized file streaming

It is a strong base for learning or experimentation, and a reasonable starting point for a small internal utility, but it is not yet a production-hardened general-purpose web server.
