use crate::{
    error::AssistantError, Interval, ServerLogFile, MAX_TIME_SPAN_IN_SECONDS, SERVER_HEALTH,
    SERVER_SOCKET_ADDRESS, TIMESTAMP_LAST_RESPONSE,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use core::panic;
use hyper::{Body, Client, Method, Request};
use log::{error, info};
use regex::Regex;
use std::collections::VecDeque;
use std::{
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    str::FromStr,
    thread::sleep,
    time::Duration,
};
use tokio::sync::RwLock;

#[derive(Debug)]
struct LogMessage {
    timestamp: DateTime<Utc>,
    _level: String,
    _service: String,
    _file: String,
    _line: u32,
    custom_message: String,
}
impl FromStr for LogMessage {
    type Err = String;

    fn from_str(log_str: &str) -> Result<Self, Self::Err> {
        // Define the regular expression pattern
        let log_regex = Regex::new(r"^\[(?P<timestamp>[^\]]+)\] \[(?P<level>[^\]]+)\] (?P<service>[^\s]+) in (?P<file>[^\:]+):(?P<line>\d+): (?P<custom_message>.*)").unwrap();

        match log_regex.captures(log_str) {
            Some(captures) => {
                // parse timestamp
                let date_str = &captures["timestamp"];
                let native_dt = NaiveDateTime::parse_from_str(date_str, "%Y-%m-%d %H:%M:%S%.3f");
                let timestamp = match native_dt {
                    Ok(native_dt) => native_dt.and_utc(),
                    Err(e) => {
                        dbg!(e.to_string());
                        panic!("Error parsing date");
                    }
                };

                Ok(LogMessage {
                    timestamp,
                    _level: captures["level"].to_string(),
                    _service: captures["service"].to_string(),
                    _file: captures["file"].to_string(),
                    _line: captures["line"].parse().ok().unwrap(),
                    custom_message: captures["custom_message"].to_string(),
                })
            }
            None => Err("Invalid API Server log message".to_string()),
        }
    }
}

pub(crate) async fn is_file<P: AsRef<Path>>(path: P) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file(),
        Err(_) => false,
    }
}

pub(crate) async fn check_server_health(
    log_file: ServerLogFile,
    interval: Interval,
) -> Result<(), AssistantError> {
    info!("Checking server health");

    let log_file_path = log_file.read().await;

    let mut file = File::open(&*log_file_path).expect("Unable to open log file");
    let file_clone = match file.try_clone() {
        Ok(file) => file,
        Err(_) => {
            let err_msg = "Unable to clone file handle";

            error!("{}", err_msg);

            return Err(AssistantError::Operation(err_msg.to_string()));
        }
    };
    let mut reader = BufReader::new(file_clone);

    // Start reading from the beginning of the file
    if let Err(e) = file.seek(SeekFrom::Start(0)) {
        let err_msg = format!("Unable to seek to start of file: {}", e);

        error!("{}", &err_msg);

        return Err(AssistantError::Operation(err_msg));
    }

    // Initialize a VecDeque with a capacity of 1
    let mut log_queue: VecDeque<LogMessage> = VecDeque::with_capacity(1);

    loop {
        info!("Checking server health");

        let mut new_lines = String::new();
        if let Err(e) = reader.read_to_string(&mut new_lines) {
            let err_msg = format!("Unable to read log file: {}", e);

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        };

        if let Some(timestamp) = TIMESTAMP_LAST_RESPONSE.get() {
            info!("Last response timestamp: {}", timestamp.read().await);
        }

        if !new_lines.is_empty() {
            info!("num of new log messages: {}", new_lines.len());

            // Iterate over the new lines and analyze server health
            // method: check the most recent pair of request and response
            for line in new_lines.lines().rev() {
                if let Ok(log_message) = LogMessage::from_str(line) {
                    if log_message.custom_message.starts_with("response_status:") {
                        // get the status code
                        let status_code = log_message
                            .custom_message
                            .split_whitespace()
                            .last()
                            .unwrap().to_string();
                        info!("Capture a response status: {}", &status_code);

                        // record the timestamp of the most recent response
                        match TIMESTAMP_LAST_RESPONSE.get() {
                            None => {TIMESTAMP_LAST_RESPONSE
                            .set(RwLock::new(log_message.timestamp))
                            .expect("Unable to set timestamp")},
                            Some(timestamp) => {
                                let mut timestamp = timestamp.write().await;

                                *timestamp = log_message.timestamp;
                            },
                        }
                        info!("Timestamp of the most recent response: {}", &log_message.timestamp);

                        match log_queue.is_empty() {
                            true => {
                                info!("Push the most recent response to the queue");
                                log_queue.push_back(log_message)},
                            false => {
                                match SERVER_HEALTH.get() {
                                    Some(_) => {
                                        let mut server_health = SERVER_HEALTH
                                        .get()
                                        .expect("Unable to get server health")
                                        .write()
                                        .await;

                                        *server_health = false;
                                    }
                                    None => {
                                        SERVER_HEALTH
                                        .set(RwLock::new(false))
                                        .expect("Unable to set server health");
                                    }
                                };

                                info!("Update the server health to false");

                                break;
                            }
                        }
                    } else if log_message.custom_message.starts_with("endpoint:") {
                        let endpoint = log_message
                            .custom_message
                            .split_whitespace()
                            .last()
                            .unwrap();
                        info!("Capture the most recent request to {}", endpoint);

                        match log_queue.is_empty() {
                            true => {
                                match SERVER_HEALTH.get() {
                                    Some(_) => {
                                        let mut server_health = SERVER_HEALTH
                                        .get()
                                        .expect("Unable to get server health")
                                        .write()
                                        .await;

                                        *server_health = false;
                                    }
                                    None => {
                                        SERVER_HEALTH
                                        .set(RwLock::new(false))
                                        .expect("Unable to set server health");
                                    }
                                };

                                info!("Update the server health to false");

                                break;
                            },
                            false => {
                                let response_log_message = log_queue.pop_front().unwrap();
                                let status_code = response_log_message.custom_message.split_whitespace().last().unwrap().to_string();
                                info!("Status code of the paired response: {}", &status_code);

                                let health_status = if status_code == "200" {
                                    true
                                } else {
                                    false
                                };

                                match SERVER_HEALTH.get() {
                                    Some(healthy) => {
                                        let mut healthy = healthy.write().await;

                                        *healthy = health_status;
                                    }
                                    None => {
                                        SERVER_HEALTH
                                            .set(RwLock::new(health_status))
                                            .expect("Unable to set server health");
                                    }
                                };

                                info!("Update the server health to {}", health_status);

                                break;
                            }
                        }
                    }
                }
            }

        } else {
            //* If long time no requests coming in, then invoke `ping_server` function to send a request to /v1/chat/completions endpoint */
            // compare the current timestamp with the last response timestamp
            if TIMESTAMP_LAST_RESPONSE.get().is_some() {
                let timestamp = TIMESTAMP_LAST_RESPONSE.get().unwrap().read().await;
                let current_timestamp = Utc::now();
                let diff = current_timestamp
                    .signed_duration_since(*timestamp)
                    .num_seconds();

                // compute the time slapsed since the last response
                info!(
                    "current: {}, last response: {}, diff: {}",
                    current_timestamp, *timestamp, diff
                );

                // if the difference is greater than 60 seconds, send a request to the server
                if diff > MAX_TIME_SPAN_IN_SECONDS {
                    // send a request to the server
                    if let Err(e) = ping_server().await {
                        if SERVER_HEALTH.get().is_none() {
                            SERVER_HEALTH
                                .set(RwLock::new(false))
                                .expect("Unable to set server health");
                        } else {
                            let mut server_health = SERVER_HEALTH
                                .get()
                                .expect("Unable to get server health")
                                .write()
                                .await;
                            if *server_health {
                                *server_health = false;
                            }
                        }

                        let err_msg = format!("{}", e);

                        error!("{}", &err_msg);

                        return Err(AssistantError::Operation(err_msg));
                    }
                }
            } else {
                // send a request to the server
                if let Err(e) = ping_server().await {
                    if SERVER_HEALTH.get().is_none() {
                        SERVER_HEALTH
                            .set(RwLock::new(false))
                            .expect("Unable to set server health");
                    } else {
                        let mut server_health = SERVER_HEALTH
                            .get()
                            .expect("Unable to get server health")
                            .write()
                            .await;
                        if *server_health {
                            *server_health = false;
                        }
                    }

                    let err_msg = format!("{}", e);

                    error!("{}", &err_msg);

                    return Err(AssistantError::Operation(err_msg));
                }
            }
        }

        // Remember the current position
        let current_position = match file.seek(SeekFrom::Current(0)) {
            Ok(position) => position,
            Err(e) => {
                let err_msg = format!("Unable to get current file position: {}", e);

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }
        };
        info!("position of current cursor: {}", current_position);

        if let Some(health) = SERVER_HEALTH.get() {
            let health = health.read().await;
            info!("Server health: {}", *health);
        }

        // Sleep for seconds specified in the interval
        let interval = interval.read().await;
        sleep(Duration::from_secs(*interval));

        // Check if there are new log entries
        if let Err(e) = file.seek(SeekFrom::End(0)) {
            let err_msg = format!("Unable to seek to end of file: {}", e);

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
        let end_position = match file.seek(SeekFrom::Current(0)) {
            Ok(position) => position,
            Err(e) => {
                let err_msg = format!("Unable to get end file position: {}", e);

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }
        };
        info!("position of end cursor: {}", end_position);

        if end_position > current_position {
            // There are new log entries, seek back to the last position
            if let Err(e) = file.seek(SeekFrom::Start(current_position)) {
                let err_msg = format!("Unable to seek to last position: {}", e);

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }
        } else {
            // No new log entries, seek back to the last position
            if let Err(e) = file.seek(SeekFrom::Start(current_position)) {
                let err_msg = format!("Unable to seek to last position: {}", e);

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }
        }
    }
}

// Send a request to the LlamaEdge API Server
async fn ping_server() -> Result<(), AssistantError> {
    info!("ping api server");

    // send a request to the LlamaEdge API Server to get the server information
    let addr = SERVER_SOCKET_ADDRESS.get().unwrap().read().await;
    let addr = (*addr).to_string();
    let url = format!("http://{}{}", addr, "/v1/chat/completions");

    let body = r###"
    {
        "messages": [
            {
                "role": "user",
                "content": "Who are you? <server-health>"
            }
        ],
        "model": "Phi-3-mini-4k-instruct",
        "stream": false
    }
    "###;

    // create a new request
    let req = match Request::builder()
        .method(Method::GET)
        .uri(&url)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
    {
        Ok(req) => req,
        Err(e) => {
            if SERVER_HEALTH.get().is_none() {
                SERVER_HEALTH
                    .set(RwLock::new(false))
                    .expect("Unable to set server health");
            } else {
                let mut server_health = SERVER_HEALTH
                    .get()
                    .expect("Unable to get server health")
                    .write()
                    .await;
                if *server_health {
                    *server_health = false;
                }
            }

            let err_msg = format!("Failed to create a request: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    };

    // send the request
    let client = Client::new();
    let response = match client.request(req).await {
        Ok(resp) => {
            info!("Sent a chat completion request to the server");

            resp
        }
        Err(e) => {
            if SERVER_HEALTH.get().is_none() {
                SERVER_HEALTH
                    .set(RwLock::new(false))
                    .expect("Unable to set server health");
            } else {
                let mut server_health = SERVER_HEALTH
                    .get()
                    .expect("Unable to get server health")
                    .write()
                    .await;
                if *server_health {
                    *server_health = false;
                }
            }

            let err_msg = format!("Failed to send a request: {}", e.to_string());

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    };

    if !response.status().is_success() {
        if SERVER_HEALTH.get().is_none() {
            if SERVER_HEALTH.set(RwLock::new(false)).is_err() {
                let err_msg = format!("Unable to set server health");

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }
        } else {
            match SERVER_HEALTH.get() {
                Some(server_health) => {
                    let mut server_health = server_health.write().await;

                    *server_health = false;

                    info!("Server health: {}", *server_health);
                }
                None => {
                    let err_msg = format!("SERVER_HEALTH is empty or not initialized");

                    error!("{}", &err_msg);

                    return Err(AssistantError::Operation(err_msg));
                }
            }
        }
    } else {
        if SERVER_HEALTH.get().is_none() {
            if SERVER_HEALTH.set(RwLock::new(true)).is_err() {
                let err_msg = format!("Unable to set server health");

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }
        } else {
            match SERVER_HEALTH.get() {
                Some(server_health) => {
                    let mut server_health = server_health.write().await;

                    *server_health = true;

                    info!("Server health: {}", *server_health);
                }
                None => {
                    let err_msg = format!("SERVER_HEALTH is empty or not initialized");

                    error!("{}", &err_msg);

                    return Err(AssistantError::Operation(err_msg));
                }
            }
        }
    }

    info!("Updated server health by sending a request to the server");

    Ok(())
}
