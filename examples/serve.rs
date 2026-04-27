use http_file_server::Server;

fn main() {
    let addr = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());

    let server = Server::bind(&addr).expect("failed to bind");
    let actual_addr = server.local_addr().unwrap();

    #[cfg(all(target_os = "linux", feature = "tls"))]
    if let (Ok(cert), Ok(key)) = (std::env::var("TLS_CERT"), std::env::var("TLS_KEY")) {
        let tls_config = http_file_server::tls::load_server_config(
            std::path::Path::new(&cert),
            std::path::Path::new(&key),
        )
        .expect("failed to load TLS config");
        println!("Serving {root:?} on https://{actual_addr}");
        server.serve_tls(&root, tls_config).expect("server error");
        return;
    }

    println!("Serving {root:?} on http://{actual_addr}");
    server.serve(&root).expect("server error");
}
