pub mod client;
pub mod error;
pub mod types;

pub use client::{
    AiClientBuilder, // Renamed
    AiLiveClient,    // Renamed
    ApiProvider,
    ServerContentContext,
    ToolHandler,
    UsageMetadataContext,
};
pub use error::GeminiError;
pub use gemini_live_macros::tool_function;
pub use types::{
    Content, FunctionDeclaration, FunctionResponse, GenerationConfig, Part, RealtimeInputConfig,
    ResponseModality, Role, Schema, SpeechConfig, SpeechLanguageCode, Tool, UsageMetadata,
};
