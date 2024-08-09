use hyper::{Body, Response};
use thiserror::Error;

#[derive(Error, Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    /// Error returned while parsing socket address failed
    #[error("Failed to parse socket address: {0}")]
    SocketAddr(String),
    /// Error returned while parsing CLI options failed
    #[error("{0}")]
    ArgumentError(String),
    /// Generic error returned while performing an operation
    #[error("{0}")]
    Operation(String),
}

pub(crate) fn internal_server_error(msg: impl AsRef<str>) -> Response<Body> {
    let err_msg = match msg.as_ref().is_empty() {
        true => "500 Internal Server Error".to_string(),
        false => format!("500 Internal Server Error: {}", msg.as_ref()),
    };

    // log error
    // error!(target: "response", "{}", &err_msg);

    Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "*")
        .header("Access-Control-Allow-Headers", "*")
        .status(hyper::StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from(err_msg))
        .unwrap()
}

pub(crate) fn invalid_endpoint(msg: impl AsRef<str>) -> Response<Body> {
    let err_msg = match msg.as_ref().is_empty() {
        true => "404 The requested service endpoint is not found".to_string(),
        false => format!(
            "404 The requested service endpoint is not found: {}",
            msg.as_ref()
        ),
    };

    // log error
    // error!(target: "response", "{}", &err_msg);

    Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "*")
        .header("Access-Control-Allow-Headers", "*")
        .status(hyper::StatusCode::NOT_FOUND)
        .body(Body::from(err_msg))
        .unwrap()
}
