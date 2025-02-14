use crate::{
    error::AssistantError, Interval, ServerLogFile, MAX_TIME_SPAN_IN_SECONDS, SERVER_HEALTH,
    SERVER_SOCKET_ADDRESS, TIMESTAMP_LAST_ACCESS_LOG,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use core::panic;
use log::{error, info, warn};
use regex::Regex;
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
    info!("Start health checker");

    let log_file_path = log_file.read().await;

    let mut file = match File::open(&*log_file_path) {
        Ok(file) => file,
        Err(e) => {
            let err_msg = e.to_string();

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    };
    let file_clone = match file.try_clone() {
        Ok(file) => file,
        Err(_) => {
            let err_msg = "Unable to clone file handle";

            error!("{}", err_msg);

            return Err(AssistantError::Operation(err_msg.to_string()));
        }
    };
    let mut reader = BufReader::new(file_clone);

    // save the current position of the cursor in the log file
    let mut current_position = 0;
    // indicate whether to check the log file. Default is true, so that the log file is checked at the beginning
    let mut can_check = true;
    let mut count = 1;
    loop {
        info!(
            ">>>>>>>>>>>>>>>>> Check health ({}) >>>>>>>>>>>>>>>>>",
            count
        );
        count += 1;

        if can_check {
            // Start reading from the beginning of the file
            if let Err(e) = file.seek(SeekFrom::Start(current_position)) {
                let err_msg = format!("Failed to seek to start of the log file: {}", e);

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }

            let mut new_lines = String::new();
            if let Err(e) = reader.read_to_string(&mut new_lines) {
                let err_msg = format!("Failed to read log messages from the log file: {}", e);

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            };
            info!("Found {} new log messages", new_lines.lines().count());

            // analyze the log messages and update the server health
            let mut updated = false;
            for line in new_lines.lines().rev() {
                if let Ok(log_message) = LogMessage::from_str(line) {
                    if log_message.custom_message.starts_with("response_status:") {
                        // get the status code
                        let status_code = log_message
                            .custom_message
                            .split_whitespace()
                            .last()
                            .unwrap()
                            .to_string();
                        info!(
                            "Found the latest response: status: {}, timestamp: {}",
                            status_code, log_message.timestamp
                        );

                        // record the timestamp of the latest response
                        match TIMESTAMP_LAST_ACCESS_LOG.get() {
                            Some(timestamp) => {
                                let mut timestamp = timestamp.write().await;

                                *timestamp = Utc::now();
                            }
                            None => {
                                TIMESTAMP_LAST_ACCESS_LOG
                                    .set(RwLock::new(Utc::now()))
                                    .expect("Failed to set TIMESTAMP_LAST_ACCESS_LOG");
                            }
                        }

                        if status_code == "500" {
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
                            };

                            info!("Update SERVER_HEALTH to false");
                        } else {
                            match SERVER_HEALTH.get() {
                                Some(server_health) => {
                                    let mut healthy = server_health.write().await;

                                    if !*healthy {
                                        *healthy = true;
                                    }
                                }
                                None => {
                                    SERVER_HEALTH
                                        .set(RwLock::new(true))
                                        .expect("Failed to set SERVER_HEALTH");
                                }
                            };

                            info!("Update SERVER_HEALTH to true");
                        }

                        updated = true;

                        break;
                    }
                }
            }

            // ping api-server if SERVER_HEALTH is not updated
            if !updated {
                info!("Ping API server");
                match ping_server().await {
                    Ok(response) => {
                        if !response.status().is_success() {
                            warn!("The response returned by the API server is not successful");
                        }

                        match SERVER_HEALTH.get() {
                            Some(server_health) => {
                                let mut healthy = server_health.write().await;

                                if !*healthy {
                                    *healthy = true;
                                }
                            }
                            None => {
                                SERVER_HEALTH
                                    .set(RwLock::new(true))
                                    .expect("Failed to set SERVER_HEALTH");
                            }
                        }

                        info!("Update SERVER_HEALTH to true");
                    }
                    Err(AssistantError::ServerDownError(_)) => {
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

                        info!("Update SERVER_HEALTH to false");
                    }
                    Err(e) => {
                        let err_msg = format!("{}", e);

                        error!("{}", &err_msg);

                        if err_msg.contains("Qdrant error:") {
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
                            };

                            info!("Update SERVER_HEALTH to false");
                        } else {
                            match SERVER_HEALTH.get() {
                                Some(server_health) => {
                                    let mut healthy = server_health.write().await;

                                    if !*healthy {
                                        *healthy = true;
                                    }
                                }
                                None => {
                                    SERVER_HEALTH
                                        .set(RwLock::new(true))
                                        .expect("Failed to set SERVER_HEALTH");
                                }
                            };

                            info!("Update SERVER_HEALTH to true");
                        }
                    }
                }
            }

            // Get the current position of the cursor in the log file
            current_position = match file.stream_position() {
                Ok(position) => position,
                Err(e) => {
                    let err_msg = format!("Unable to get current file position: {}", e);

                    error!("{}", &err_msg);

                    return Err(AssistantError::Operation(err_msg));
                }
            };
            // warn!("The position of current cursor: {}", current_position);
        } else {
            info!("Not found new log messages");

            //* If long time no requests coming in, then invoke `ping_server` function to send a request to /v1/chat/completions endpoint */
            match TIMESTAMP_LAST_ACCESS_LOG.get() {
                Some(timestamp) => {
                    let diff = {
                        let timestamp = timestamp.read().await;
                        let current_timestamp = Utc::now();
                        current_timestamp
                            .signed_duration_since(*timestamp)
                            .num_seconds()
                    };

                    // compute the time slapsed since the last response
                    info!("Time elapsed: {} secs", diff);

                    // if the difference is greater than 60 seconds, send a request to the API server
                    if diff >= MAX_TIME_SPAN_IN_SECONDS {
                        // update TIMESTAMP_LAST_ACCESS_LOG
                        let mut timestamp = timestamp.write().await;
                        *timestamp = Utc::now();

                        info!("Ping API server");
                        match ping_server().await {
                            Ok(response) => {
                                if !response.status().is_success() {
                                    warn!(
                                        "The response returned by the API server is not successful"
                                    );

                                    // get the body of the response in string format
                                    match response.text().await {
                                        Ok(body_text) => {
                                            let err_msg = body_text;

                                            warn!("{}", &err_msg);

                                            if err_msg.contains("Qdrant error:") {
                                                match SERVER_HEALTH.get() {
                                                    Some(server_health) => {
                                                        let mut healthy =
                                                            server_health.write().await;

                                                        if *healthy {
                                                            *healthy = false;
                                                        }
                                                    }
                                                    None => {
                                                        SERVER_HEALTH
                                                            .set(RwLock::new(false))
                                                            .expect("Failed to set SERVER_HEALTH");
                                                    }
                                                };

                                                info!("Update SERVER_HEALTH to false");
                                            } else {
                                                match SERVER_HEALTH.get() {
                                                    Some(server_health) => {
                                                        let mut healthy =
                                                            server_health.write().await;

                                                        if !*healthy {
                                                            *healthy = true;
                                                        }
                                                    }
                                                    None => {
                                                        SERVER_HEALTH
                                                            .set(RwLock::new(true))
                                                            .expect("Failed to set SERVER_HEALTH");
                                                    }
                                                };

                                                info!("Update SERVER_HEALTH to true");
                                            }
                                        }
                                        Err(e) => {
                                            let err_msg = format!(
                                                "Failed to get the body of the response: {}",
                                                e
                                            );

                                            error!("{}", &err_msg);
                                        }
                                    }
                                } else {
                                    match SERVER_HEALTH.get() {
                                        Some(server_health) => {
                                            let mut healthy = server_health.write().await;

                                            if !*healthy {
                                                *healthy = true;
                                            }
                                        }
                                        None => {
                                            SERVER_HEALTH
                                                .set(RwLock::new(true))
                                                .expect("Failed to set SERVER_HEALTH");
                                        }
                                    }

                                    info!("Update SERVER_HEALTH to true");
                                }
                            }
                            Err(AssistantError::ServerDownError(err_msg)) => {
                                error!("{}", &err_msg);

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

                                info!("Update SERVER_HEALTH to false");
                            }
                            Err(e) => {
                                let err_msg = format!("{}", e);

                                error!("{}", &err_msg);

                                if err_msg.contains("Qdrant error:") {
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
                                    };

                                    info!("Update SERVER_HEALTH to false");
                                } else {
                                    match SERVER_HEALTH.get() {
                                        Some(server_health) => {
                                            let mut healthy = server_health.write().await;

                                            if !*healthy {
                                                *healthy = true;
                                            }
                                        }
                                        None => {
                                            SERVER_HEALTH
                                                .set(RwLock::new(true))
                                                .expect("Failed to set SERVER_HEALTH");
                                        }
                                    };

                                    info!("Update SERVER_HEALTH to true");
                                }
                            }
                        }
                    }
                }
                None => {
                    // update TIMESTAMP_LAST_ACCESS_LOG
                    TIMESTAMP_LAST_ACCESS_LOG
                        .set(RwLock::new(Utc::now()))
                        .expect("Failed to set TIMESTAMP_LAST_ACCESS_LOG");

                    info!("Ping API server");
                    match ping_server().await {
                        Ok(response) => {
                            if !response.status().is_success() {
                                warn!("The response returned by the API server is not successful");
                            }

                            match SERVER_HEALTH.get() {
                                Some(server_health) => {
                                    let mut healthy = server_health.write().await;

                                    if !*healthy {
                                        *healthy = true;
                                    }
                                }
                                None => {
                                    SERVER_HEALTH
                                        .set(RwLock::new(true))
                                        .expect("Failed to set SERVER_HEALTH");
                                }
                            }

                            info!("Update SERVER_HEALTH to true");
                        }
                        Err(AssistantError::ServerDownError(_)) => {
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

                            info!("Update SERVER_HEALTH to false");
                        }
                        Err(e) => {
                            let err_msg = format!("{}", e);

                            error!("{}", &err_msg);

                            if err_msg.contains("Qdrant error:") {
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
                                };

                                info!("Update SERVER_HEALTH to false");
                            } else {
                                match SERVER_HEALTH.get() {
                                    Some(server_health) => {
                                        let mut healthy = server_health.write().await;

                                        if !*healthy {
                                            *healthy = true;
                                        }
                                    }
                                    None => {
                                        SERVER_HEALTH
                                            .set(RwLock::new(true))
                                            .expect("Failed to set SERVER_HEALTH");
                                    }
                                };

                                info!("Update SERVER_HEALTH to true");
                            }
                        }
                    }
                }
            }
        }

        // print the server health
        if let Some(health) = SERVER_HEALTH.get() {
            let health = health.read().await;
            info!("Server health: {}", *health);
        }

        // Sleep for seconds specified in the interval
        let interval = interval.read().await;
        sleep(Duration::from_secs(*interval));

        // Check if there are new log entries
        // Get the end position
        let latest_position = match file.seek(SeekFrom::End(0)) {
            Ok(position) => position,
            Err(e) => {
                let err_msg = format!(
                    "Failed to get the latest position of the cursor in the log file: {}",
                    e
                );

                error!("{}", &err_msg);

                return Err(AssistantError::Operation(err_msg));
            }
        };

        // Check if there are new log entries
        can_check = latest_position > current_position;

        // seek back to the last position for the next iteration
        if let Err(e) = file.seek(SeekFrom::Start(current_position)) {
            let err_msg = format!("Failed to set back the cursor to the last position: {}", e);

            error!("{}", &err_msg);

            return Err(AssistantError::Operation(err_msg));
        }
    }
}

// Send a request to the LlamaEdge API Server
async fn ping_server() -> Result<reqwest::Response, AssistantError> {
    let addr = SERVER_SOCKET_ADDRESS.get().unwrap().read().await;
    let addr = (*addr).to_string();
    let url = format!("http://{}{}", addr, "/v1/chat/completions");

    let client = reqwest::Client::new();
    match client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "messages": [{
                "role": "user",
                "content": "Who are you? <server-health>"
            }],
            "model": "Phi-3-mini-4k-instruct",
            "stream": false
        }))
        .send()
        .await
    {
        Ok(resp) => {
            info!("Received response from the API server");
            Ok(resp)
        }
        Err(e) => {
            let err_msg = e.to_string();

            error!("Response error: {}", &err_msg);

            Err(AssistantError::ServerDownError(err_msg))
        }
    }
}
