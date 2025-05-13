// Modules for internal organization
mod backend;
pub mod builder;
mod connection;
mod gemini_backend; // New backend module
mod handle;
mod handlers;
mod openai_backend; // New backend module // Keep builder public

// Re-export necessary public types
pub use backend::{ApiError, ApiProvider, AppState, BackendConfig, UnifiedServerEvent}; // Export key backend types
pub use builder::GeminiLiveClientBuilder;
pub use handle::GeminiLiveClient;
pub use handlers::{ServerContentContext, ToolHandler, UsageMetadataContext}; // Export necessary handlers/contexts
