pub mod client;
pub mod error;
pub mod types;

// Re-export key components
pub use client::{
    ApiProvider, // Export provider enum
    AppState,    // Export AppState trait
    GeminiLiveClient,
    GeminiLiveClientBuilder,
    ServerContentContext,
    ToolHandler, // ToolHandler now uses Arc<dyn AppState>
    UsageMetadataContext,
};
pub use error::GeminiError;
pub use types::{
    Content,
    FunctionDeclaration,
    FunctionResponse,
    GenerationConfig,
    Part,
    RealtimeInputConfig,
    ResponseModality,
    Role,
    Schema,
    SpeechConfig,
    SpeechLanguageCode,
    Tool, // Added Tool, FR, RIC
    UsageMetadata, // Added UsageMetadata
          // Add other commonly used types
};

// Macro export
pub use gemini_live_macros::tool_function;
