use hyper::client::HttpConnector;
use hyper::{
    service::{make_service_fn, service_fn},
    Body, Client, Method, Request, Response, Server, Uri,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;
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

// Periodically send notifications to all subscribers
async fn periodic_notifications(subscribers: Subscribers) {
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

// Forward the request to the target server and return the response to the client
// async fn forward(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
//     let client = Client::new();
//     client.request(req).await
// }

async fn forward(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let client = Client::new();

    // retrieve the path and query from the original request
    let path_and_query = req.uri().path_and_query().map(|x| x.as_str()).unwrap_or("");
    // construct forwarding uri
    let uri_string = format!("http://localhost:10086{}", path_and_query);
    let uri = Uri::try_from(uri_string).unwrap();

    // decompose original request
    let (parts, body) = req.into_parts();
    // construct forwarded request
    let mut forwarded_req = Request::from_parts(parts, body);
    let forwarded_req_uri = forwarded_req.uri_mut();
    *forwarded_req_uri = uri;

    // send forwarded request
    client.request(forwarded_req).await
}

// Handle the incoming request
async fn handle_request(
    mut req: Request<Body>,
    subscribers: Subscribers,
) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();
    if path.starts_with("/v1") {
        println!("API Path: {}", path);

        forward(req).await
    } else if path == "/subscribe" {
        println!("Notification Path: {}", path);

        let body_bytes = hyper::body::to_bytes(req.body_mut()).await?;
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        let url = body_str.trim().to_string();

        let mut subs = subscribers.write().await;
        subs.insert(url.clone());
        println!("Subscribed: {}", url);

        Ok(Response::new(Body::from("Subscribed")))
    } else if path == "/unsubscribe" {
        println!("Notification Path: {}", path);

        let body_bytes = hyper::body::to_bytes(req.body_mut()).await?;
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        let url = body_str.trim().to_string();

        let mut subs = subscribers.write().await;
        subs.remove(&url);
        println!("Unsubscribed: {}", url);

        Ok(Response::new(Body::from("Unsubscribed")))
    } else {
        println!("Invalid Path: {}", path);

        Ok(Response::new(Body::from("Invalid endpoint")))
    }
}

#[tokio::main]
async fn main() {
    let subscribers: Subscribers = Arc::new(RwLock::new(HashSet::new()));
    // let message = "Hello, this is a test notification!";

    let subscribers_clone = Arc::clone(&subscribers);
    tokio::spawn(async move {
        periodic_notifications(subscribers_clone).await;
    });

    let make_svc = make_service_fn(move |_| {
        let subscribers = Arc::clone(&subscribers);
        async {
            Ok::<_, hyper::Error>(service_fn(move |req| {
                handle_request(req, Arc::clone(&subscribers))
            }))
        }
    });

    // Run it with hyper on localhost:3000
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Listening on http://{}", addr);

    let server = Server::bind(&addr).serve(make_svc);

    if let Err(e) = server.await {
        println!("server error: {}", e);
    }
}
