use anyhow::Result;
use hyper::{client::HttpConnector, Body, Client, Method, Request};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

type Subscribers = Arc<RwLock<HashSet<String>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Notification {
    message: String,
}
unsafe impl Send for Notification {}
unsafe impl Sync for Notification {}

// Periodically send notifications to all subscribers
pub(crate) async fn periodic_notifications(subscribers: Subscribers) {
    let client = Client::new();
    let mut interval = interval(Duration::from_secs(10));
    let message = Notification {
        message: "Hello, this is a test notification!".to_string(),
    };
    loop {
        interval.tick().await;
        let subs = subscribers.read().await;
        for url in subs.iter() {
            if let Err(e) = send_notification(&client, url, message.clone()).await {
                eprintln!("Error sending notification to {}: {}", url, e);
            }
        }
    }
}

// Send a notification to a subscriber
async fn send_notification(
    client: &Client<HttpConnector>,
    url: &str,
    message: Notification,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = serde_json::to_string(&message)?;
    let req = Request::builder()
        .method(Method::POST)
        .uri(url)
        .header("Content-Type", "application/json")
        .body(Body::from(payload.to_string()))?;
    let resp = client.request(req).await?;
    if resp.status().is_success() {
        println!("Notification sent to {} successfully!", url);
    } else {
        println!(
            "Failed to send notification to {}. Status: {}",
            url,
            resp.status()
        );
    }
    Ok(())
}
