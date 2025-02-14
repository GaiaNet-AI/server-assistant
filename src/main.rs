mod error;
mod health;

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Parser;
use error::AssistantError;
use health::{check_server_health, is_file};
use log::{debug, error, info, warn};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashSet, fs::File, io::Write, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::{sync::RwLock, time::Duration};

type Subscribers = Arc<RwLock<HashSet<String>>>;
pub(crate) type ServerLogFile = Arc<RwLock<String>>;
pub(crate) type Interval = Arc<RwLock<u64>>;

// default socket address of LlamaEdge API Server instance
const DEFAULT_SERVER_SOCKET_ADDRESS: &str = "0.0.0.0:8080";
pub(crate) const MAX_TIME_SPAN_IN_SECONDS: i64 = 30;

// server info
pub(crate) static SERVER_INFO: OnceCell<RwLock<Value>> = OnceCell::new();
// server health
static SERVER_HEALTH: OnceCell<RwLock<bool>> = OnceCell::new();
// timestamp of the last response
pub(crate) static TIMESTAMP_LAST_ACCESS_LOG: OnceCell<RwLock<DateTime<Utc>>> = OnceCell::new();
pub(crate) static SERVER_SOCKET_ADDRESS: OnceCell<RwLock<SocketAddr>> = OnceCell::new();

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
    /// Path to gaianet directory
    #[arg(long, required = true)]
    gaianet_dir: PathBuf,
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
    if let Err(addr) = SERVER_SOCKET_ADDRESS.set(RwLock::new(server_addr)) {
        let addr = addr.read().await;
        let err_msg = format!("Failed to store the server address: {}", (*addr));

        error!("{}", &err_msg);

        return Err(AssistantError::Operation(err_msg));
    }

    let server_log_file = cli.gaianet_dir.join("log").join("start-llamaedge.log");
    if !server_log_file.exists() || !is_file(&server_log_file).await {
        let err_msg = format!("Invalid log file path: {}", &server_log_file.display());
        error!("{}", &err_msg);
        return Err(AssistantError::ArgumentError(err_msg));
    }
    info!("Log file of API server: {}", &server_log_file.display());
    let server_log_file: ServerLogFile =
        Arc::new(RwLock::new(server_log_file.to_string_lossy().to_string()));

    // get device id from frpc.toml
    let frpc_toml = cli.gaianet_dir.join("gaia-frp").join("frpc.toml");
    if !is_file(&frpc_toml).await {
        error!(
            "Invalid frpc.toml file path: {}",
            &frpc_toml.to_string_lossy()
        );
        return Err(AssistantError::ArgumentError(format!(
            "Invalid frpc.toml file path: {}",
            &frpc_toml.to_string_lossy()
        )));
    }
    let toml_content = match tokio::fs::read_to_string(&frpc_toml).await {
        Ok(content) => content,
        Err(e) => {
            error!(
                "Failed to read the content of frpc.toml file: {}",
                e.to_string()
            );
            return Err(AssistantError::Operation(format!(
                "Failed to read the content of frpc.toml file: {}",
                e
            )));
        }
    };
    let toml_value: toml::Value = match toml::from_str(&toml_content) {
        Ok(value) => value,
        Err(e) => {
            error!(
                "Failed to parse the content of frpc.toml file: {}",
                e.to_string()
            );
            return Err(AssistantError::Operation(format!(
                "Failed to parse the content of frpc.toml file: {}",
                e
            )));
        }
    };
    let device_id = match toml_value.get("metadatas") {
        Some(metadata) => match metadata.get("deviceId") {
            Some(device_id) => match device_id.as_str() {
                Some(device_id) => device_id.to_string(),
                None => {
                    error!("Failed to get the device id from frpc.toml file.");
                    return Err(AssistantError::Operation(
                        "Failed to get the device id from frpc.toml file.".to_string(),
                    ));
                }
            },
            None => {
                error!("Failed to get the device id from frpc.toml file.");
                return Err(AssistantError::Operation(
                    "Failed to get the device id from frpc.toml file.".to_string(),
                ));
            }
        },
        None => {
            error!("Failed to get the metadatas from frpc.toml file.");
            return Err(AssistantError::Operation(
                "Failed to get the metadatas from frpc.toml file.".to_string(),
            ));
        }
    };
    info!("Device ID: {}", &device_id);

    // get domain from config.json
    let config_json = cli.gaianet_dir.join("config.json");
    if !is_file(&config_json).await {
        error!(
            "Invalid config.json file path: {}",
            &config_json.to_string_lossy()
        );
        return Err(AssistantError::ArgumentError(format!(
            "Invalid config.json file path: {}",
            &config_json.to_string_lossy()
        )));
    }
    let config_content = match tokio::fs::read_to_string(&config_json).await {
        Ok(content) => content,
        Err(e) => {
            error!(
                "Failed to read the content of config.json file: {}",
                e.to_string()
            );
            return Err(AssistantError::Operation(format!(
                "Failed to read the content of config.json file: {}",
                e
            )));
        }
    };
    let config_value: serde_json::Value = match serde_json::from_str(&config_content) {
        Ok(value) => value,
        Err(e) => {
            error!(
                "Failed to parse the content of config.json file: {}",
                e.to_string()
            );
            return Err(AssistantError::Operation(format!(
                "Failed to parse the content of config.json file: {}",
                e
            )));
        }
    };
    let domain = match config_value["domain"].as_str() {
        Some(domain) => domain.to_string(),
        None => {
            error!("Failed to get the domain from config.json file.");
            return Err(AssistantError::Operation(
                "Failed to get the domain from config.json file.".to_string(),
            ));
        }
    };
    info!("Domain: {}", &domain);

    let server_info_url = format!("https://hub.domain.{}/device-info/{}", &domain, &device_id);

    let server_health_url = format!(
        "https://hub.domain.{}/device-health/{}",
        &domain, &device_id
    );

    // compute sha256 of chat model
    let mut sha256_chat_model = String::new();
    if let Some(chat_url) = config_value["chat"].as_str() {
        if !chat_url.is_empty() {
            if let Some(chat_model_name) = chat_url.split("/").last() {
                let chat_model_name = chat_model_name.to_string();
                let chat_model = cli.gaianet_dir.join(&chat_model_name);
                if chat_model.exists() {
                    // compute sha256 of chat model
                    if let Ok(hash) = sha256::try_digest(chat_model) {
                        sha256_chat_model = hash;
                        info!("sha256 of chat model: {}", &sha256_chat_model);
                    }
                }
            }
        }
    }

    // compute sha256 of embedding model
    let mut sha256_embedding_model = String::new();
    if let Some(embedding_url) = config_value["embedding"].as_str() {
        if !embedding_url.is_empty() {
            if let Some(embedding_model_name) = embedding_url.split("/").last() {
                let embedding_model_name = embedding_model_name.to_string();
                let embedding_model = cli.gaianet_dir.join(&embedding_model_name);
                if embedding_model.exists() {
                    // compute sha256 of embedding model
                    if let Ok(hash) = sha256::try_digest(embedding_model) {
                        sha256_embedding_model = hash;
                        info!("sha256 of embedding model: {}", &sha256_embedding_model);
                    }
                }
            }
        }
    }

    // parse the interval of checking server health
    let interval = cli.interval;
    info!("Interval of checking server health: {}", &interval);
    let interval: Interval = Arc::new(RwLock::new(interval));

    // parse the system prompt
    let mut system_prompt = String::new();
    if let Some(prompt) = config_value["system_prompt"].as_str() {
        if !prompt.is_empty() {
            system_prompt = prompt.to_string();
        }
    }
    info!("System prompt: {}", &system_prompt);

    // parse the rag prompt
    let mut rag_prompt = String::new();
    if let Some(prompt) = config_value["rag_prompt"].as_str() {
        if !prompt.is_empty() {
            rag_prompt = prompt.to_string();
        }
    }
    info!("RAG prompt: {}", &rag_prompt);

    // add subscribers for server info
    let server_info_subscribers: Subscribers = Arc::new(RwLock::new(HashSet::new()));
    info!("Add subscriber for server info: {}", &server_info_url);
    server_info_subscribers
        .write()
        .await
        .insert(server_info_url);

    let push_info_handle = tokio::spawn(async move {
        // retrieve server information
        retrieve_server_info(
            &system_prompt,
            &rag_prompt,
            &sha256_chat_model,
            &sha256_embedding_model,
        )
        .await?;

        // push server information to all subscribers
        match push_server_info(server_info_subscribers.clone()).await {
            Ok(_) => {
                info!("Server information sent to subscribers successfully!");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("Failed to push server info to subscribers. {}", e);

                error!("{}", &err_msg);

                Err(AssistantError::Operation(err_msg))
            }
        }
    });

    // add subscribers for server health
    let server_health_subscribers: Subscribers = Arc::new(RwLock::new(HashSet::new()));
    info!("Add subscriber for server health: {}", &server_health_url);
    server_health_subscribers
        .write()
        .await
        .insert(server_health_url);

    // check server health periodically
    let server_log_file_clone = Arc::clone(&server_log_file);
    let interval_clone = Arc::clone(&interval);
    let health_check_handle = tokio::spawn(async move {
        if let Err(e) = check_server_health(server_log_file_clone, interval_clone).await {
            match SERVER_HEALTH.get() {
                Some(server_health) => {
                    let mut healthy = server_health.write().await;

                    if *healthy {
                        *healthy = false;
                    }
                }
                None => {
                    SERVER_HEALTH
                        .set(RwLock::new(false))
                        .expect("Failed to set SERVER_HEALTH");
                }
            }

            let err_msg = format!("Failed to check server health: {}", e);

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }

        Ok(())
    });

    // push server health periodically
    let server_health_subscribers_clone = Arc::clone(&server_health_subscribers);
    let interval_clone = Arc::clone(&interval);
    let health_notify_handle = tokio::spawn(async move {
        periodic_notifications(server_health_subscribers_clone, interval_clone).await;
    });

    if let Err(e) = tokio::try_join!(push_info_handle, health_check_handle, health_notify_handle) {
        let err_msg = format!("Failed to check server health: {}", e);

        error!("{}", &err_msg);

        return Err(AssistantError::Operation(err_msg));
    }

    Ok(())
}

// Retrieve server information from the LlamaEdge API Server
async fn retrieve_server_info(
    system_prompt: impl AsRef<str>,
    rag_prompt: impl AsRef<str>,
    sha256_chat_model: impl AsRef<str>,
    sha256_embedding_model: impl AsRef<str>,
) -> Result<(), AssistantError> {
    // send a request to the LlamaEdge API Server to get the server information
    let addr = SERVER_SOCKET_ADDRESS
        .get()
        .ok_or_else(|| AssistantError::Operation("Failed to get the server address".to_string()))?
        .read()
        .await;
    let addr = (*addr).to_string();
    // Convert 0.0.0.0 to localhost
    let addr = addr.replace("0.0.0.0", "localhost");
    let url = format!("http://{}{}", addr, "/v1/info");

    info!("Retrieving server information from: {}", &url);

    // create a new reqwest client
    let client = reqwest::Client::new();
    let response = match client.get(&url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            let err_msg = format!("Failed to send a request: {}", e);
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

    // parse the response
    let mut server_info = match response.json::<serde_json::Value>().await {
        Ok(json) => json,
        Err(e) => {
            let err_msg = format!("Failed to parse the response: {}", e);
            error!("{}", &err_msg);
            return Err(AssistantError::Operation(err_msg));
        }
    };
    debug!("raw server info: {}", server_info.to_string());

    // get the server type
    let server_type = match server_info["api_server"]["type"].as_str() {
        Some(server_type) => server_type.to_string(),
        None => {
            let err_msg = "Failed to get the server type.".to_string();
            error!("{}", &err_msg);
            return Err(AssistantError::Operation(err_msg));
        }
    };
    info!("server type: {}", server_type);

    // add the rag prompt to the server information if the server type is `rag`
    if server_type == "rag" {
        if let Some(map) = server_info.as_object_mut() {
            info!(
                "insert rag prompt to server info: {}",
                system_prompt.as_ref()
            );
            map.insert(
                "rag_prompt".to_string(),
                serde_json::Value::String(rag_prompt.as_ref().to_string()),
            );
        }
    }

    // add the system prompt to the server information
    if let Some(extra) = server_info["extras"].as_object_mut() {
        info!(
            "insert system prompt to server info: {}",
            system_prompt.as_ref()
        );

        extra.insert(
            "system_prompt".to_string(),
            serde_json::Value::String(system_prompt.as_ref().to_string()),
        );
    }

    // add sha256 of chat model to the server information
    if let Some(map) = server_info["chat_model"].as_object_mut() {
        if !sha256_chat_model.as_ref().is_empty() {
            map.insert(
                "sha256".to_string(),
                serde_json::Value::String(sha256_chat_model.as_ref().to_string()),
            );
        }
    }

    // add sha256 of embedding model to the server information
    if let Some(map) = server_info["embedding_model"].as_object_mut() {
        if !sha256_embedding_model.as_ref().is_empty() {
            map.insert(
                "sha256".to_string(),
                serde_json::Value::String(sha256_embedding_model.as_ref().to_string()),
            );
        }
    }

    // get system info
    match system_info_lite::get_system_info() {
        Ok(system_info) => {
            info!("hardware info: {:?}", system_info);
            let sys_info = serde_json::to_value(system_info).unwrap();

            // add hardware info to the server information
            if let Some(map) = server_info.as_object_mut() {
                map.insert("hardware".to_string(), sys_info);
            }
        }
        Err(e) => {
            error!("Failed to get system info: {}", e.to_string());
        }
    }

    info!("set SERVER_INFO: {}", server_info.to_string());

    // store the server information
    if SERVER_INFO.set(RwLock::new(server_info)).is_err() {
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

            let server_info_str = match serde_json::to_string(&*server_info) {
                Ok(info) => info,
                Err(e) => {
                    let err_msg = format!("Failed to serialize the server information. {}", e);
                    error!("{}", &err_msg);
                    return Err(AssistantError::Operation(err_msg));
                }
            };

            // Create a client using reqwest
            let client = reqwest::Client::new();

            for url in subs.iter() {
                let mut retry = 0;

                // retry 3 times if the request fails to send
                loop {
                    info!("tries ({}) to send server info to {}", retry, &url);

                    // send request using reqwest
                    let response = match client
                        .post(url.to_string())
                        .header("Content-Type", "application/json")
                        .body(server_info_str.clone())
                        .send()
                        .await
                    {
                        Ok(resp) => resp,
                        Err(e) => {
                            retry += 1;
                            if retry >= 3 {
                                let err_msg = format!(
                                    "Failed to send server information to {}: {}",
                                    &url, e,
                                );
                                error!("{}", &err_msg);
                                return Err(AssistantError::Operation(err_msg));
                            } else {
                                let err_msg = format!(
                                    "Failed to send server information to {}: {}. Retrying ({})...",
                                    &url, e, retry
                                );
                                warn!("{}", &err_msg);
                                continue;
                            }
                        }
                    };

                    // check if the request was successful
                    if response.status().is_success() {
                        info!("Server info sent to {} successfully!", &url);
                        break;
                    } else {
                        retry += 1;
                        if retry >= 3 {
                            error!("Failed to get server information from {}.", &url);
                            break;
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
async fn _push_server_health(
    client: &reqwest::Client,
    url: &str,
    message: Notification,
) -> Result<(), AssistantError> {
    let payload = match serde_json::to_string(&message) {
        Ok(payload) => payload,
        Err(e) => {
            let err_msg = format!("Failed to serialize the message: {}", e);
            error!("{}", &err_msg);
            return Err(AssistantError::Operation(err_msg));
        }
    };
    info!("health status: {}", payload);

    // Send POST request using reqwest
    match client
        .post(url)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().is_success() {
                info!("Server health sent to {} successfully!", url);
            } else {
                error!(
                    "Failed to send server health to {}. Status: {}",
                    url,
                    resp.status()
                );
            }
        }
        Err(e) => {
            let err_msg = format!("Failed to send request: {}", e);
            error!("{}", &err_msg);
            return Err(AssistantError::Operation(err_msg));
        }
    }

    Ok(())
}

// Periodically send notifications to all subscribers
async fn periodic_notifications(subscribers: Subscribers, interval: Interval) {
    // Create a reusable reqwest client
    let client = reqwest::Client::new();

    let interval = interval.read().await;
    let mut interval = tokio::time::interval(Duration::from_secs(*interval));
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
                info!("Not found subscribers to notifications.");
            }
            false => {
                info!("Sending notifications to all subscribers...");

                for url in subs.iter() {
                    // Send POST request using reqwest
                    match client.post(url).json(&message).send().await {
                        Ok(response) => {
                            if !response.status().is_success() {
                                error!(
                                    "Failed to send notification to {}. Status: {}",
                                    url,
                                    response.status()
                                );
                            }
                        }
                        Err(e) => {
                            error!("Error sending notification to {}: {}", url, e);
                        }
                    }
                }

                info!("Notification sent to all subscribers successfully!");
            }
        }
    }
}
