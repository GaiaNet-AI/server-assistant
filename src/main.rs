mod error;
mod health;

use anyhow::Result;
use clap::Parser;
use error::AssistantError;
use health::{check_server_health, is_file};
use hyper::{client::HttpConnector, Body, Client, Method, Request, Response};
use hyper_tls::HttpsConnector;
use log::{error, info, warn};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashSet, fs::File, io::Write, net::SocketAddr, sync::Arc};
use tokio::{
    sync::RwLock,
    time::{interval, Duration},
};

type Subscribers = Arc<RwLock<HashSet<String>>>;
type ServerSocketAddr = Arc<RwLock<SocketAddr>>;
pub(crate) type ServerLogFile = Arc<RwLock<String>>;
pub(crate) type Interval = Arc<RwLock<u64>>;

// default socket address of LlamaEdge API Server instance
const DEFAULT_SERVER_SOCKET_ADDRESS: &str = "0.0.0.0:8080";

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
async fn main() -> Result<(), AssistantError> {
    // parse the command line arguments
    let cli = Cli::parse();

    // create a new log file
    let file = match File::create(&cli.log) {
        Ok(file) => file,
        Err(e) => {
            error!("Failed to create log file: {}", e);

            return Err(AssistantError::Operation(format!(
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

    // parse socket address of LlamaEdge API Server instance
    let server_addr = cli
        .server_socket_addr
        .parse::<SocketAddr>()
        .map_err(|e| AssistantError::SocketAddr(e.to_string()))?;
    info!("Socket address of API server: {}", &server_addr);
    let server_addr = Arc::new(RwLock::new(server_addr));

    // parse the path to the api server log file
    let server_log_file = cli.server_log_file;
    if !is_file(&server_log_file).await {
        error!("Invalid log file path: {}", &server_log_file);
        return Err(AssistantError::ArgumentError(format!(
            "Invalid log file path: {}",
            &server_log_file
        )));
    }
    info!("Log file of API server: {}", &server_log_file);
    let server_log_file: ServerLogFile = Arc::new(RwLock::new(server_log_file));

    // parse the interval of checking server health
    let interval = cli.interval;
    info!("Interval of checking server health: {}", &interval);
    let interval: Interval = Arc::new(RwLock::new(interval));

    // todo: set subscribers for server info
    let server_info_subscribers: Subscribers = Arc::new(RwLock::new(HashSet::new()));

    // todo: set subscribers for server health
    let server_health_subscribers: Subscribers = Arc::new(RwLock::new(HashSet::new()));

    // check server health periodically
    let server_log_file_clone = Arc::clone(&server_log_file);
    let interval_clone = Arc::clone(&interval);
    tokio::spawn(async move {
        if let Err(e) = check_server_health(server_log_file_clone, interval_clone).await {
            let err_msg = format!("Failed to check server health: {}", e);

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }

        Ok(())
    });

    // push server health periodically
    let server_health_subscribers_clone = Arc::clone(&server_health_subscribers);
    tokio::spawn(async move {
        periodic_notifications(server_health_subscribers_clone).await;
    });

    // retrieve server information
    retrieve_server_info(Arc::clone(&server_addr)).await?;

    // push server information to all subscribers
    match push_server_info(server_info_subscribers.clone()).await {
        Ok(_) => info!("Server information sent to subscribers successfully!"),
        Err(e) => {
            let err_msg = format!(
                "Failed to push server info to subscribers. {}",
                e.to_string()
            );

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    }

    Ok(())
}

// Retrieve server information from the LlamaEdge API Server
async fn retrieve_server_info(socket_addr: ServerSocketAddr) -> Result<(), AssistantError> {
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

            return Err(AssistantError::Operation(err_msg));
        }
    };

    // send the request
    let client = Client::new();
    let response = match client.request(req).await {
        Ok(resp) => resp,
        Err(e) => {
            let err_msg = format!("Failed to send a request: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    };
    if !response.status().is_success() {
        let err_msg = format!(
            "Failed to get server info from API Server. Status: {}",
            response.status()
        );

        error!("{}", &err_msg);

        return Err(AssistantError::Operation(err_msg));
    }

    // parse the server information from the response
    let body_bytes = match hyper::body::to_bytes(response.into_body()).await {
        Ok(bytes) => bytes,
        Err(e) => {
            let err_msg = format!("Failed to read the body of the response: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
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

            return Err(AssistantError::Operation(err_msg));
        }
    };
    info!("Server Information: {}", server_info.to_string());

    // store the server information
    if let Err(_) = SERVER_INFO.set(RwLock::new(server_info)) {
        let err_msg = "Failed to store the server information.";

        error!("{}", err_msg);

        return Err(AssistantError::Operation(err_msg.to_string()));
    }

    Ok(())
}

// Push server information to all subscribers
async fn push_server_info(subscribers: Subscribers) -> Result<(), AssistantError> {
    let subs = subscribers.read().await;
    match subs.is_empty() {
        true => {
            let err_msg = "No subscribers found.".to_string();

            error!("{}", &err_msg);

            Err(AssistantError::Operation(err_msg))
        }
        false => {
            let server_info = match SERVER_INFO.get() {
                Some(info) => info,
                None => {
                    return Err(AssistantError::Operation(
                        "No server info available.".to_string(),
                    ))
                }
            };
            let server_info = server_info.read().await;

            // create an HTTPS connector
            let https = HttpsConnector::new();

            // send the request
            let client = Client::builder().build::<_, Body>(https);

            let server_info_str = match serde_json::to_string(&*server_info) {
                Ok(info) => info,
                Err(e) => {
                    let err_msg = format!(
                        "Failed to serialize the server information. {}",
                        e.to_string()
                    );

                    error!("{}", &err_msg);

                    return Err(AssistantError::Operation(err_msg));
                }
            };

            for url in subs.iter() {
                let mut retry = 0;
                let mut response: Response<Body>;

                // retry 3 times if the request fails to send
                loop {
                    // create a new request
                    let req = match Request::builder()
                        .method(Method::POST)
                        .uri(url.to_string())
                        .header("Content-Type", "application/json")
                        .body(Body::from(server_info_str.clone()))
                    {
                        Ok(req) => req,
                        Err(e) => {
                            let err_msg = format!("Failed to create a request. {}", e.to_string());

                            error!("{}", &err_msg);

                            return Err(AssistantError::Operation(err_msg));
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

                                return Err(AssistantError::Operation(err_msg));
                            } else {
                                let err_msg = format!(
                                    "Failed to send server information to {}: {}. Retrying ({})...",
                                    &url,
                                    e.to_string(),
                                    retry
                                );

                                warn!("{}", &err_msg);
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
            }

            Ok(())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Notification {
    health: bool,
}
unsafe impl Send for Notification {}
unsafe impl Sync for Notification {}

// Send a notification to a subscriber
async fn send_health_status(
    client: &Client<HttpConnector>,
    url: &str,
    message: Notification,
) -> Result<(), AssistantError> {
    let payload = match serde_json::to_string(&message) {
        Ok(payload) => payload,
        Err(e) => {
            let err_msg = format!("Failed to serialize the message: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    };

    // create a new request
    let req = match Request::builder()
        .method(Method::POST)
        .uri(url)
        .header("Content-Type", "application/json")
        .body(Body::from(payload.to_string()))
    {
        Ok(req) => req,
        Err(e) => {
            let err_msg = format!("Failed to create a request: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    };

    match client.request(req).await {
        Ok(resp) => match resp.status().is_success() {
            true => {
                info!("Notification sent to {} successfully!", url)
            }
            false => error!(
                "Failed to send notification to {}. Status: {}",
                url,
                resp.status()
            ),
        },
        Err(e) => {
            let err_msg = format!("Failed to send a request: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    }

    Ok(())
}

// Periodically send notifications to all subscribers
async fn periodic_notifications(subscribers: Subscribers) {
    let client = Client::new();
    let mut interval = interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let health = match SERVER_HEALTH.get() {
            Some(health) => {
                let health = health.read().await;
                *health
            }
            None => continue,
        };
        let message = Notification { health };
        let subs = subscribers.read().await;
        match subs.is_empty() {
            true => {
                info!("Not found subsribers to notifications.");
            }
            false => {
                info!("Sending notifications to all subscribers...");

                for url in subs.iter() {
                    if let Err(e) = send_health_status(&client, url, message.clone()).await {
                        error!("Error sending notification to {}: {}", url, e);
                    }
                }

                info!("Notification sent to all subscribers successfully!");
            }
        }
    }
}
