mod error;
mod notification;

use anyhow::Result;
use clap::Parser;
use error::ServerError;
use hyper::{
    service::{make_service_fn, service_fn},
    Body, Client, Request, Response, Server, Uri,
};
use notification::periodic_notifications;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

type Subscribers = Arc<RwLock<HashSet<String>>>;
type ServerSocketAddr = Arc<RwLock<SocketAddr>>;

// default socket address of LlamaEdge API Server instance
const DEFAULT_SERVER_SOCKET_ADDRESS: &str = "0.0.0.0:8080";
// default socket address of server assistant
const DEFAULT_ASSISTANT_SOCKET_ADDRESS: &str = "0.0.0.0:3000";

#[derive(Debug, Parser)]
#[command(name = "Server Assistant", version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"), about = "An assistant for LlamaEdge API Server")]
struct Cli {
    /// Socket address of LlamaEdge API Server instance
    #[arg(short, long, default_value = DEFAULT_SERVER_SOCKET_ADDRESS)]
    api_server_socket_addr: String,
    /// Socket address of server assistant
    #[arg(long, default_value = DEFAULT_ASSISTANT_SOCKET_ADDRESS)]
    socket_addr: String,
    /// Interval in seconds for sending notifications
    #[arg(short, long, default_value = "10")]
    interval: u64,
}

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    // parse the command line arguments
    let cli = Cli::parse();

    // socket address
    let server_addr = cli
        .api_server_socket_addr
        .parse::<SocketAddr>()
        .map_err(|e| ServerError::SocketAddr(e.to_string()))?;
    let server_addr = Arc::new(RwLock::new(server_addr));

    // assistant socket address
    let assistant_addr = cli
        .socket_addr
        .parse::<SocketAddr>()
        .map_err(|e| ServerError::SocketAddr(e.to_string()))?;

    // notification interval
    let interval = cli.interval;

    let subscribers: Subscribers = Arc::new(RwLock::new(HashSet::new()));

    let subscribers_clone = Arc::clone(&subscribers);
    tokio::spawn(async move {
        periodic_notifications(subscribers_clone, interval).await;
    });

    let make_svc = make_service_fn(move |_| {
        let subscribers = Arc::clone(&subscribers);
        let server_addr = Arc::clone(&server_addr);
        // let server_addr = Arc::clone(&server_addr);
        async {
            Ok::<_, hyper::Error>(service_fn(move |req| {
                handle_request(req, Arc::clone(&subscribers), Arc::clone(&server_addr))
            }))
        }
    });

    // Run it with hyper on localhost:3000
    println!("Listening on http://{}", &assistant_addr);

    let server = Server::bind(&assistant_addr).serve(make_svc);

    match server.await {
        Ok(_) => Ok(()),
        Err(e) => Err(ServerError::Operation(e.to_string())),
    }
}

// Handle the incoming request
async fn handle_request(
    mut req: Request<Body>,
    subscribers: Subscribers,
    socket_addr: ServerSocketAddr,
) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();
    let response = if path.starts_with("/v1") {
        println!("API Path: {}", path);

        forward(req, socket_addr).await
    } else if path == "/subscribe" {
        println!("Notification Path: {}", path);

        let body_bytes = match hyper::body::to_bytes(req.body_mut()).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(error::internal_server_error(format!(
                    "Failed to read the body of the subscribe request: {}",
                    e.to_string()
                )))
            }
        };
        let body_str = match String::from_utf8(body_bytes.to_vec()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(error::internal_server_error(format!(
                    "Failed to parse the body of the subscribe request: {}",
                    e.to_string()
                )))
            }
        };
        let url = body_str.trim().to_string();

        let mut subs = subscribers.write().await;
        subs.insert(url.clone());
        println!("Subscribed: {}", url);

        Response::new(Body::from("Subscribed"))
    } else if path == "/unsubscribe" {
        println!("Notification Path: {}", path);

        let body_bytes = match hyper::body::to_bytes(req.body_mut()).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(error::internal_server_error(format!(
                    "Failed to read the body of the unsubscribe request: {}",
                    e.to_string()
                )))
            }
        };
        let body_str = match String::from_utf8(body_bytes.to_vec()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(error::internal_server_error(format!(
                    "Failed to parse the body of the unsubscribe request: {}",
                    e.to_string()
                )))
            }
        };
        let url = body_str.trim().to_string();

        let mut subs = subscribers.write().await;
        subs.remove(&url);
        println!("Unsubscribed: {}", url);

        Response::new(Body::from("Unsubscribed"))
    } else {
        println!("Invalid Path: {}", path);

        error::invalid_endpoint("Invalid endpoint")
    };

    Ok(response)
}

// Forward API requests to the LlamaEdge API Server
async fn forward(req: Request<Body>, socket_addr: ServerSocketAddr) -> Response<Body> {
    let client = Client::new();

    // retrieve the path and query from the original request
    let path_and_query = req.uri().path_and_query().map(|x| x.as_str()).unwrap_or("");
    // construct forwarding uri
    let addr = socket_addr.read().await;
    let addr = (*addr).to_string();
    let uri_string = format!("http://{}{}", addr, path_and_query);
    let uri = match Uri::try_from(uri_string) {
        Ok(uri) => uri,
        Err(e) => {
            return error::internal_server_error(format!("Failed to parse URI: {}", e.to_string()))
        }
    };
    println!("Forwarding request to: {}", uri);

    // decompose original request
    let (parts, body) = req.into_parts();
    // construct forwarded request
    let mut forwarded_req = Request::from_parts(parts, body);
    // update the uri of the forwarded request
    let forwarded_req_uri = forwarded_req.uri_mut();
    *forwarded_req_uri = uri;

    // send forwarded request
    match client.request(forwarded_req).await {
        Ok(resp) => resp,
        Err(e) => {
            error::internal_server_error(format!("Failed to forward request. {}", e.to_string()))
        }
    }
}
