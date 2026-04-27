use http_file_server::Server;

fn main() {
    let addr = "127.0.0.1:8080";
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());

    let server = Server::bind(addr).expect("failed to bind");
    println!("Serving {root:?} on http://{addr}");
    server.serve(&root).expect("server error");
}
