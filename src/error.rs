use thiserror::Error;
use tokio_tungstenite::tungstenite;

#[derive(Error, Debug)]
pub enum GeminiError {
    #[error("WebSocket connection error: {0}")]
    WebSocketError(#[from] tungstenite::Error),

    #[error("JSON serialization/deserialization error: {0}")]
    SerdeError(#[from] serde_json::Error),

    #[error("I/O error: {0}")] // Keep for potential file ops later?
    IoError(#[from] std::io::Error),

    #[error("API communication error: {0}")] // More general API error
    ApiError(String),

    #[error("Configuration error: {0}")] // Added for setup issues
    ConfigurationError(String),

    #[error("Connection not established or setup not complete")]
    NotReady,

    #[error("Message from server was not in expected format")]
    UnexpectedMessage, // May be less relevant with specific backend parsing

    #[error("Attempted to send message on a closed connection")]
    ConnectionClosed,

    #[error("Function call handler not found for: {0}")]
    FunctionHandlerNotFound(String), // Keep for tool handling

    #[error("Error sending message to internal task: Channel closed")] // Clarified SendError
    SendError,

    #[error("Missing API key")]
    MissingApiKey, // Keep for initial setup

    #[error("Feature or operation not supported by the selected backend: {0}")] // Added
    UnsupportedOperation(String),

    #[error("Error during message deserialization: {0}")] // Added for specific parsing issues
    DeserializationError(String),

    #[error("HTTP error during connection setup: {0}")] // Added for HTTP errors
    HttpError(#[from] http::Error),

    #[error("URL parsing error: {0}")] // Added for URL errors
    UrlError(#[from] url::ParseError),
}
