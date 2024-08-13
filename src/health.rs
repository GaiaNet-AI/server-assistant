use crate::{Interval, ServerLogFile, SERVER_HEALTH};
use chrono::{DateTime, NaiveDateTime, Utc};
use core::panic;
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
    level: String,
    service: String,
    file: String,
    line: u32,
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
                    level: captures["level"].to_string(),
                    service: captures["service"].to_string(),
                    file: captures["file"].to_string(),
                    line: captures["line"].parse().ok().unwrap(),
                    custom_message: captures["custom_message"].to_string(),
                })
            }
            None => Err("Invalid API Server log message".to_string()),
        }
    }
}

async fn is_file<P: AsRef<Path>>(path: P) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file(),
        Err(_) => false,
    }
}

pub(crate) async fn check_server_health(log_file: ServerLogFile, interval: Interval) {
    let log_file_path = log_file.read().await;
    // let log_file_path = "/Users/sam/gaianet/log/start-llamaedge.log";
    println!("Log file path: {}", log_file_path);

    if !is_file(&*log_file_path).await {
        panic!("Invalid log file path");
    }

    let mut file = File::open(&*log_file_path).expect("Unable to open log file");
    let mut reader = BufReader::new(file.try_clone().expect("Unable to clone file handle"));

    // Start reading from the beginning of the file
    file.seek(SeekFrom::Start(0))
        .expect("Unable to seek to start of file");

    // Initialize a VecDeque with a capacity of 1
    let mut log_queue: VecDeque<LogMessage> = VecDeque::with_capacity(1);

    loop {
        let mut new_lines = String::new();
        reader
            .read_to_string(&mut new_lines)
            .expect("Unable to read new lines");

        for line in new_lines.lines() {
            if let Ok(log_message) = LogMessage::from_str(line) {
                if log_message.custom_message.starts_with("endpoint") {
                    println!("{:#?}", log_message);
                    log_queue.push_back(log_message);

                    if log_queue.len() > 1 {
                        log_queue.pop_front();
                    }
                } else if log_message
                    .custom_message
                    .starts_with("response_is_success")
                {
                    println!("{:#?}", log_message);
                    log_queue.push_back(log_message);

                    if log_queue.len() > 1 {
                        if let Some(log) = log_queue.pop_front() {
                            if !log.custom_message.starts_with("endpoint") {
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

                                    *server_health = false;
                                }
                            } else {
                                if SERVER_HEALTH.get().is_none() {
                                    SERVER_HEALTH
                                        .set(RwLock::new(true))
                                        .expect("Unable to set server health");
                                } else {
                                    let mut server_health = SERVER_HEALTH
                                        .get()
                                        .expect("Unable to get server health")
                                        .write()
                                        .await;

                                    *server_health = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Remember the current position
        let current_position = file
            .seek(SeekFrom::Current(0))
            .expect("Unable to get current file position");

        println!("Current position: {}", current_position);

        println!(
            "Server health: {:?}",
            SERVER_HEALTH.get().unwrap().read().await
        );

        // Sleep for seconds specified in the interval
        let interval = interval.read().await;
        sleep(Duration::from_secs(*interval));

        // Check if there are new log entries
        file.seek(SeekFrom::End(0))
            .expect("Unable to seek to end of file");
        let end_position = file
            .seek(SeekFrom::Current(0))
            .expect("Unable to get end file position");

        println!("End position: {}", end_position);

        if end_position > current_position {
            // There are new log entries, seek back to the last position
            file.seek(SeekFrom::Start(current_position))
                .expect("Unable to seek to last position");
        } else {
            // No new log entries, seek back to the last position
            file.seek(SeekFrom::Start(current_position))
                .expect("Unable to seek to last position");
        }
    }
}
