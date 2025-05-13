// src/client/mod.rs
// Modules for internal organization
mod backend;
pub mod builder;
mod connection;
mod gemini_backend;
pub mod handle;
pub mod handlers;
mod openai_backend;

// Re-export necessary public types
pub use backend::{ApiError, ApiProvider, BackendConfig, UnifiedServerEvent};
pub use builder::AiClientBuilder;
pub use handle::AiLiveClient; // Corrected from GeminiLiveClient
pub use handlers::{ServerContentContext, ToolHandler, UsageMetadataContext};
