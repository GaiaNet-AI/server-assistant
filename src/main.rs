mod error;
mod health;
// mod notification;

use anyhow::Result;
use clap::Parser;
use error::ServerError;
use health::check_server_health;
use hyper::{
    // client::HttpConnector,
    // server,
    service::{make_service_fn, service_fn},
    Body,
    Client,
    Method,
    Request,
    Response,
    Server,
    Uri,
};
// use notification::periodic_notifications;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::Value;
// use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

// type Subscribers = Arc<RwLock<HashSet<String>>>;
type ServerSocketAddr = Arc<RwLock<SocketAddr>>;
type ServerInfoTargetUrl = Arc<RwLock<String>>;
pub(crate) type ServerLogFile = Arc<RwLock<String>>;
pub(crate) type Interval = Arc<RwLock<u64>>;

// default socket address of LlamaEdge API Server instance
const DEFAULT_SERVER_SOCKET_ADDRESS: &str = "0.0.0.0:8080";
// default socket address of server assistant
const DEFAULT_ASSISTANT_SOCKET_ADDRESS: &str = "0.0.0.0:3000";
// Interval for checking server log
// const INTERVAL: Duration = Duration::from_secs(10);

// server info
pub(crate) static SERVER_INFO: OnceCell<RwLock<Value>> = OnceCell::new();
// server health
static SERVER_HEALTH: OnceCell<RwLock<bool>> = OnceCell::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Payload {
    message: String,
}

#[derive(Debug, Parser)]
#[command(name = "Server Assistant", version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"), about = "An assistant for LlamaEdge API Server")]
struct Cli {
    /// Socket address of LlamaEdge API Server instance
    #[arg(short, long, default_value = DEFAULT_SERVER_SOCKET_ADDRESS)]
    api_server_socket_addr: String,
    /// Socket address of server assistant
    #[arg(long, default_value = DEFAULT_ASSISTANT_SOCKET_ADDRESS)]
    socket_addr: String,
    /// Target URL for sending server information
    #[arg(long)]
    target_url: String,
    /// Interval in seconds for sending notifications
    #[arg(short, long, default_value = "10")]
    interval: u64,
    /// Path to the `start-llamaedge.log` file
    #[arg(long)]
    server_log_file: String,
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

    // target URL
    let target_url = cli.target_url;
    let target_url: ServerInfoTargetUrl = Arc::new(RwLock::new(target_url));

    // notification interval
    let interval = cli.interval;
    let interval: Interval = Arc::new(RwLock::new(interval));

    // server log file
    let server_log_file = cli.server_log_file;
    let server_log_file: ServerLogFile = Arc::new(RwLock::new(server_log_file));

    // retrieve server information
    retrieve_server_info(Arc::clone(&server_addr)).await?;

    // let subscribers: Subscribers = Arc::new(RwLock::new(HashSet::new()));

    // let subscribers_clone = Arc::clone(&subscribers);
    let server_log_file_clone = Arc::clone(&server_log_file);
    let interval_clone = Arc::clone(&interval);
    tokio::spawn(async move {
        check_server_health(server_log_file_clone, interval_clone).await;
        // periodic_notifications(subscribers_clone, interval).await;
    });

    let make_svc = make_service_fn(move |_| {
        // let subscribers = Arc::clone(&subscribers);
        let server_addr = Arc::clone(&server_addr);
        let target_url = Arc::clone(&target_url);
        // let server_addr = Arc::clone(&server_addr);
        async {
            Ok::<_, hyper::Error>(service_fn(move |req| {
                handle_request(
                    req,
                    // Arc::clone(&subscribers),
                    Arc::clone(&server_addr),
                    Arc::clone(&target_url),
                )
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
    req: Request<Body>,
    // subscribers: Subscribers,
    socket_addr: ServerSocketAddr,
    target_url: ServerInfoTargetUrl,
) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();
    let response = if path == "/v1/info" {
        println!("API Path: {}", path);

        // retrieve the target URL
        let url = target_url.read().await;
        println!("Target URL: {}", &url);

        let server_info = match SERVER_INFO.get() {
            Some(info) => info,
            None => {
                let response = forward(req, socket_addr).await;

                // parse the server information from the response
                let body_bytes = match hyper::body::to_bytes(response.into_body()).await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        return Ok(error::internal_server_error(format!(
                            "Failed to read the body of the response: {}",
                            e.to_string()
                        )))
                    }
                };
                let server_info = match serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                    Ok(json) => json,
                    Err(e) => {
                        return Ok(error::internal_server_error(format!(
                            "Failed to parse the body of the response: {}",
                            e.to_string()
                        )))
                    }
                };
                println!("Server Information: {:?}", server_info);

                // store the server information
                if let Err(_) = SERVER_INFO.set(RwLock::new(server_info)) {
                    return Ok(error::internal_server_error(
                        "Failed to store the server information".to_string(),
                    ));
                }

                SERVER_INFO.get().unwrap()
            }
        };
        let server_info = server_info.read().await;

        println!("Server Information: {:?}", &server_info);

        // send the request
        let client = Client::new();

        let mut retry = 0;
        let mut response: Response<Body>;
        loop {
            // create a new request
            let req = match Request::builder()
                .method(Method::GET)
                .uri(url.to_string())
                .header("Content-Type", "application/json")
                .body(Body::from(server_info.to_string()))
            {
                Ok(req) => req,
                Err(e) => {
                    return Ok(error::internal_server_error(format!(
                        "Failed to create a request: {}",
                        e.to_string()
                    )))
                }
            };

            println!("tries ({}) to send server info to {}", retry, &url);
            // send the request
            response = match client.request(req).await {
                Ok(resp) => resp,
                Err(e) => {
                    retry += 1;
                    if retry >= 3 {
                        return Ok(error::internal_server_error(format!(
                            "Failed to send server information to {}: {}",
                            &url,
                            e.to_string()
                        )));
                    }
                    continue;
                }
            };

            // check if the request was successful
            match response.status().is_success() {
                true => {
                    println!("Server information sent to {} successfully!", &url);
                    break;
                }
                false => {
                    retry += 1;
                    if retry >= 3 {
                        println!("Failed to get server information from {}.", &url);
                        break;
                    }
                }
            }
        }

        response
    } else if path.starts_with("/v1") {
        println!("API Path: {}", path);

        forward(req, socket_addr).await
    } else if path == "/health" {
        println!("Check server health");

        // check if the server is healthy
        if let Some(health) = SERVER_HEALTH.get() {
            let health = health.read().await;
            if !*health {
                error::internal_server_error("Server is not healthy")
            } else {
                // return response
                let result = Response::builder()
                    .header("Access-Control-Allow-Origin", "*")
                    .header("Access-Control-Allow-Methods", "*")
                    .header("Access-Control-Allow-Headers", "*")
                    .header("Content-Type", "application/json")
                    .body(Body::from("OK"));
                match result {
                    Ok(response) => response,
                    Err(e) => {
                        let err_msg = e.to_string();

                        // log
                        // error!(target: "stdout", "{}", &err_msg);

                        error::internal_server_error(err_msg)
                    }
                }
            }
        } else {
            error::internal_server_error("Server is not healthy")
        }
    } else if path == "/subscribe" {
        {
            // println!("Notification Path: {}", path);

            // let body_bytes = match hyper::body::to_bytes(req.body_mut()).await {
            //     Ok(bytes) => bytes,
            //     Err(e) => {
            //         return Ok(error::internal_server_error(format!(
            //             "Failed to read the body of the subscribe request: {}",
            //             e.to_string()
            //         )))
            //     }
            // };
            // let body_str = match String::from_utf8(body_bytes.to_vec()) {
            //     Ok(s) => s,
            //     Err(e) => {
            //         return Ok(error::internal_server_error(format!(
            //             "Failed to parse the body of the subscribe request: {}",
            //             e.to_string()
            //         )))
            //     }
            // };
            // let url = body_str.trim().to_string();

            // let mut subs = subscribers.write().await;
            // subs.insert(url.clone());
            // println!("Subscribed: {}", url);

            // Response::new(Body::from("Subscribed"))
        }

        unimplemented!("Subscribe endpoint is not implemented")
    } else if path == "/unsubscribe" {
        {
            // println!("Notification Path: {}", path);

            // let body_bytes = match hyper::body::to_bytes(req.body_mut()).await {
            //     Ok(bytes) => bytes,
            //     Err(e) => {
            //         return Ok(error::internal_server_error(format!(
            //             "Failed to read the body of the unsubscribe request: {}",
            //             e.to_string()
            //         )))
            //     }
            // };
            // let body_str = match String::from_utf8(body_bytes.to_vec()) {
            //     Ok(s) => s,
            //     Err(e) => {
            //         return Ok(error::internal_server_error(format!(
            //             "Failed to parse the body of the unsubscribe request: {}",
            //             e.to_string()
            //         )))
            //     }
            // };
            // let url = body_str.trim().to_string();

            // let mut subs = subscribers.write().await;
            // subs.remove(&url);
            // println!("Unsubscribed: {}", url);

            // Response::new(Body::from("Unsubscribed"))
        }

        unimplemented!("Unsubscribe endpoint is not implemented")
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

async fn retrieve_server_info(socket_addr: ServerSocketAddr) -> Result<(), ServerError> {
    // send a request to the LlamaEdge API Server to get the server information
    let addr = socket_addr.read().await;
    let addr = (*addr).to_string();
    let url = format!("http://{}{}", addr, "/v1/info");

    // create a new request
    let req = match Request::builder()
        .method(Method::GET)
        .uri(&url)
        .header("Content-Type", "application/json")
        .body(Body::empty())
    {
        Ok(req) => req,
        Err(e) => {
            return Err(ServerError::Operation(format!(
                "Failed to create a request: {}",
                e.to_string()
            )));
        }
    };

    // send the request
    let client = Client::new();
    let response = match client.request(req).await {
        Ok(resp) => resp,
        Err(e) => {
            return Err(ServerError::Operation(format!(
                "Failed to send a request: {}",
                e.to_string()
            )));
        }
    };
    if !response.status().is_success() {
        return Err(ServerError::Operation(format!(
            "Failed to get server info from API Server. Status: {}",
            response.status()
        )));
    }

    // parse the server information from the response
    let body_bytes = match hyper::body::to_bytes(response.into_body()).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return Err(ServerError::Operation(format!(
                "Failed to read the body of the response: {}",
                e.to_string()
            )));
        }
    };
    let server_info = match serde_json::from_slice::<serde_json::Value>(&body_bytes) {
        Ok(json) => json,
        Err(e) => {
            return Err(ServerError::Operation(format!(
                "Failed to parse the body of the response: {}",
                e.to_string()
            )));
        }
    };
    println!("Server Information: {:?}", &server_info);

    // store the server information
    if let Err(_) = SERVER_INFO.set(RwLock::new(server_info)) {
        return Err(ServerError::Operation(
            "Failed to store the server information".to_string(),
        ));
    }

    Ok(())
}
