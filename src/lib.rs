pub mod client;
pub mod error;
pub mod types;

pub use client::{
    GeminiLiveClient, GeminiLiveClientBuilder, ServerContentContext, ToolHandler,
    UsageMetadataContext,
};
pub use error::GeminiError;
pub use types::{
    Content, FunctionDeclaration, GenerationConfig, Part, ResponseModality, Role, Schema,
    SpeechConfig, SpeechLanguageCode,
};

pub use gemini_live_macros::tool_function;
