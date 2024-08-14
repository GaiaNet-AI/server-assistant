mod error;
mod health;
// mod notification;

use anyhow::Result;
use clap::Parser;
use error::ServerError;
use health::{check_server_health, is_file};
use hyper::{
    service::{make_service_fn, service_fn},
    Body, Client, Method, Request, Response, Server, Uri,
};
// use notification::periodic_notifications;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::Value;
// use std::collections::HashSet;
use log::{error, info};
use std::{fs::File, io::Write, net::SocketAddr, sync::Arc};
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
    /// Socket address of server assistant
    #[arg(long, default_value = DEFAULT_ASSISTANT_SOCKET_ADDRESS)]
    socket_addr: String,
    /// Socket address of LlamaEdge API Server instance
    #[arg(long, default_value = DEFAULT_SERVER_SOCKET_ADDRESS)]
    server_socket_addr: String,
    /// Path to the `start-llamaedge.log` file
    #[arg(long)]
    server_log_file: String,
    /// Target URL for sending server information
    #[arg(long)]
    target_url: String,
    /// Interval in seconds for sending notifications
    #[arg(short, long, default_value = "10")]
    interval: u64,
    /// log file
    #[arg(long, default_value = "assistant.log")]
    log: String,
}

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    // parse the command line arguments
    let cli = Cli::parse();

    // create a new log file
    let file = match File::create(&cli.log) {
        Ok(file) => file,
        Err(e) => {
            error!("Failed to create log file: {}", e);

            return Err(ServerError::Operation(format!(
                "Failed to create log file: {}",
                e
            )));
        }
    };

    // initialize the logger
    let target = Box::new(file);
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(target))
        .format(|buf, record| {
            writeln!(
                buf,
                "{} [{}] {} in {}:{}: {}",
                chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                record.level(),
                record.module_path().unwrap_or("unknown"),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.args()
            )
        })
        .init();
    info!("log file of server assistant: {}", &cli.log);

    // parse socket address of server assistant
    let assistant_addr = cli
        .socket_addr
        .parse::<SocketAddr>()
        .map_err(|e| ServerError::SocketAddr(e.to_string()))?;
    info!("Socket address of server assistant: {}", &assistant_addr);

    // parse socket address of LlamaEdge API Server instance
    let server_addr = cli
        .server_socket_addr
        .parse::<SocketAddr>()
        .map_err(|e| ServerError::SocketAddr(e.to_string()))?;
    info!("Socket address of API server: {}", &server_addr);
    let server_addr = Arc::new(RwLock::new(server_addr));

    // parse the path to the api server log file
    let server_log_file = cli.server_log_file;
    if !is_file(&server_log_file).await {
        error!("Invalid log file path: {}", &server_log_file);
        return Err(ServerError::ArgumentError(format!(
            "Invalid log file path: {}",
            &server_log_file
        )));
    }
    info!("Log file of API server: {}", &server_log_file);
    let server_log_file: ServerLogFile = Arc::new(RwLock::new(server_log_file));

    // parse the target URL for sending server information
    let target_url = cli.target_url;
    info!("Target URL for sending server info: {}", &target_url);
    let target_url: ServerInfoTargetUrl = Arc::new(RwLock::new(target_url));

    // parse the interval of checking server health
    let interval = cli.interval;
    info!("Interval of checking server health: {}", &interval);
    let interval: Interval = Arc::new(RwLock::new(interval));

    // retrieve server information
    retrieve_server_info(Arc::clone(&server_addr)).await?;

    let server_log_file_clone = Arc::clone(&server_log_file);
    let interval_clone = Arc::clone(&interval);
    tokio::spawn(async move {
        if let Err(e) = check_server_health(server_log_file_clone, interval_clone).await {
            let err_msg = format!("Failed to check server health: {}", e);

            error!("{}", &err_msg);

            return Err(ServerError::Operation(err_msg));
        }

        Ok(())
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
    info!("Listening on http://{}", &assistant_addr);

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
        info!("Server Information Requested");

        // retrieve the target URL
        let url = target_url.read().await;
        info!("target URL: {}", &url);

        let server_info = match SERVER_INFO.get() {
            Some(info) => info,
            None => {
                let response = forward(req, socket_addr).await;

                // parse the server information from the response
                let body_bytes = match hyper::body::to_bytes(response.into_body()).await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        let err_msg =
                            format!("Failed to read the body of the response: {}", e.to_string());

                        error!("{}", &err_msg);

                        return Ok(error::internal_server_error(err_msg));
                    }
                };
                let server_info = match serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                    Ok(json) => json,
                    Err(e) => {
                        let err_msg = format!(
                            "Failed to parse the body of the response: {}",
                            e.to_string()
                        );

                        error!("{}", &err_msg);

                        return Ok(error::internal_server_error(err_msg));
                    }
                };
                info!("Server Information: {}", server_info.to_string());

                // store the server information
                if let Err(_) = SERVER_INFO.set(RwLock::new(server_info)) {
                    let err_msg = "Failed to store the server information.";

                    error!("{}", err_msg);

                    return Ok(error::internal_server_error(err_msg));
                }

                SERVER_INFO.get().unwrap()
            }
        };
        let server_info = server_info.read().await;
        info!("Server Information: {:?}", &server_info);

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
                    let err_msg = format!("Failed to create a request: {}", e.to_string());

                    error!("{}", &err_msg);

                    return Ok(error::internal_server_error(err_msg));
                }
            };

            info!("tries ({}) to send server info to {}", retry, &url);
            // send the request
            response = match client.request(req).await {
                Ok(resp) => resp,
                Err(e) => {
                    retry += 1;
                    if retry >= 3 {
                        let err_msg = format!(
                            "Failed to send server information to {}: {}",
                            &url,
                            e.to_string()
                        );

                        error!("{}", &err_msg);

                        return Ok(error::internal_server_error(err_msg));
                    }
                    continue;
                }
            };

            // check if the request was successful
            match response.status().is_success() {
                true => {
                    info!("Server information sent to {} successfully!", &url);
                    break;
                }
                false => {
                    retry += 1;
                    if retry >= 3 {
                        error!("Failed to get server information from {}.", &url);
                        break;
                    }
                }
            }
        }

        response
    } else if path.starts_with("/v1") {
        info!("Forward request to the endpoint: {}", path);

        forward(req, socket_addr).await
    } else if path == "/health" {
        info!("Receive a request in health endpoint");

        // check if the server is healthy
        if let Some(health) = SERVER_HEALTH.get() {
            let health = health.read().await;
            if !*health {
                error!("Server is not healthy");

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

                        error!(target: "stdout", "{}", &err_msg);

                        error::internal_server_error(err_msg)
                    }
                }
            }
        } else {
            error!("Server is not healthy");

            error::internal_server_error("Server is not healthy")
        }
    } else {
        let err_msg = format!("Invalid Path: {}", path);

        error!("{}", &err_msg);

        error::invalid_endpoint(err_msg)
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
            let err_msg = format!("Failed to parse URI: {}", e.to_string());

            error!("{}", &err_msg);

            return error::internal_server_error(err_msg);
        }
    };
    info!("Forwarding request to: {}", uri);

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
            let err_msg = format!("Failed to forward request: {}", e.to_string());

            error!("{}", &err_msg);

            error::internal_server_error(err_msg)
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
            let err_msg = format!("Failed to create a request: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(ServerError::Operation(err_msg));
        }
    };

    // send the request
    let client = Client::new();
    let response = match client.request(req).await {
        Ok(resp) => resp,
        Err(e) => {
            let err_msg = format!("Failed to send a request: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(ServerError::Operation(err_msg));
        }
    };
    if !response.status().is_success() {
        let err_msg = format!(
            "Failed to get server info from API Server. Status: {}",
            response.status()
        );

        error!("{}", &err_msg);

        return Err(ServerError::Operation(err_msg));
    }

    // parse the server information from the response
    let body_bytes = match hyper::body::to_bytes(response.into_body()).await {
        Ok(bytes) => bytes,
        Err(e) => {
            let err_msg = format!("Failed to read the body of the response: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(ServerError::Operation(err_msg));
        }
    };
    let server_info = match serde_json::from_slice::<serde_json::Value>(&body_bytes) {
        Ok(json) => json,
        Err(e) => {
            let err_msg = format!(
                "Failed to parse the body of the response: {}",
                e.to_string()
            );

            error!("{}", &err_msg);

            return Err(ServerError::Operation(err_msg));
        }
    };
    info!("Server Information: {}", server_info.to_string());

    // store the server information
    if let Err(_) = SERVER_INFO.set(RwLock::new(server_info)) {
        let err_msg = "Failed to store the server information.";

        error!("{}", err_msg);

        return Err(ServerError::Operation(err_msg.to_string()));
    }

    Ok(())
}
