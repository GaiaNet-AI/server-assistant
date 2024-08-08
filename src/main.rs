use hyper::{service::service_fn, Body, Client, Request, Response, Server};
use std::net::SocketAddr;
use tower::make::Shared;

async fn log(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();

    if path.starts_with("/v1") {
        println!("API Path: {}", path);
    } else {
        println!("Generic Path: {}", path);
    }

    handle(req).await
}

async fn handle(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let client = Client::new();
    client.request(req).await
}

#[tokio::main]
async fn main() {
    let make_service = Shared::new(service_fn(log));

    // Run it with hyper on localhost:3000
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Listening on http://{}", addr);

    let server = Server::bind(&addr).serve(make_service);

    if let Err(e) = server.await {
        println!("server error: {}", e);
    }
}
