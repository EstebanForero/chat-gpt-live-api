use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::HashMap; // Added for ToolDefinition

// --- Existing Structs (Keep as they are generally useful) ---
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpeechLanguageCode {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpeechConfig {/* ... unchanged ... */}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResponseModality {/* ... unchanged ... */}
impl Serialize for ResponseModality {
    /* ... */
}
impl<'de> Deserialize<'de> for ResponseModality {
    /* ... */
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Content {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Part {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableCode {/* ... unchanged ... */}

// --- Modified / Added Structs ---

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>, // Keep as Value for flexibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>, // ID for tracking, used by OpenAI and Gemini
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)] // Added Default
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    pub name: String,    // Name of the function called
    pub response: Value, // Result from the function, should be JSON-serializable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>, // ID to correlate with the FunctionCall (essential for OpenAI)
}

// Removed: BidiGenerateContentToolCall (Internal to Gemini backend)
// Removed: BidiGenerateContentToolResponse (Internal to Gemini backend)

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActivityHandling {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StartSensitivity {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EndSensitivity {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TurnCoverage {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Blob {/* ... unchanged ... */} // Still used by Gemini backend

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {/* ... unchanged ... */}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)] // Added Default
#[serde(rename_all = "camelCase")]
pub struct Tool {
    // Primarily used by Gemini's setup structure
    pub function_declarations: Vec<FunctionDeclaration>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)] // Added Default
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    // Gemini's format for declaring functions
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Schema>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)] // Added Default
#[serde(rename_all = "camelCase")]
pub struct Schema {
    // Gemini's schema format
    #[serde(rename = "type")]
    pub schema_type: String, // e.g., "OBJECT", "STRING", "NUMBER"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, Schema>>, // For OBJECT type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>, // For OBJECT type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    // Add other schema fields if needed (e.g., items for ARRAY)
}

// --- Structs primarily for Gemini Backend ---
// These might become internal implementation details of gemini_backend.rs

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentSetup {
    // Changed to pub(crate)
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>, // Gemini specific Tool struct
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_input_config: Option<RealtimeInputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resumption: Option<SessionResumptionConfig>, // Keep if needed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_compression: Option<ContextWindowCompressionConfig>, // Keep if needed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_audio_transcription: Option<AudioTranscriptionConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionConfig {} // Simple marker struct for now

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentClientContent {
    // Changed to pub(crate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<Vec<Content>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<bool>,
}

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentRealtimeInput {
    // Changed to pub(crate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Blob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<Blob>, // Keep video placeholder
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_start: Option<ActivityStart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_end: Option<ActivityEnd>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_stream_end: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStart {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEnd {}

// Gemini specific client payload enum
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ClientMessagePayload {
    // Changed to pub(crate)
    Setup(BidiGenerateContentSetup),
    ClientContent(BidiGenerateContentClientContent),
    RealtimeInput(BidiGenerateContentRealtimeInput),
    ToolResponse(BidiGenerateContentToolResponse), // Reference internal struct
}

// Gemini specific tool response struct
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentToolResponse {
    // Changed to pub(crate)
    pub function_responses: Vec<FunctionResponse>, // Uses the unified FunctionResponse
}

// --- Structs primarily for Gemini Backend Parsing ---
// These might become internal implementation details of gemini_backend.rs

#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentSetupComplete {} // Changed to pub(crate)

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentServerContent {
    // Changed to pub(crate)
    #[serde(default)]
    pub generation_complete: bool,
    #[serde(default)]
    pub turn_complete: bool,
    #[serde(default)]
    pub interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grounding_metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_transcription: Option<BidiGenerateContentTranscription>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_turn: Option<Content>, // Uses the common Content struct
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentTranscription {
    // Changed to pub(crate)
    pub text: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentToolCall {
    // Changed to pub(crate)
    pub function_calls: Vec<FunctionCall>, // Uses the common FunctionCall struct
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentToolCallCancellation {
    // Changed to pub(crate)
    pub ids: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoAway {
    // Changed to pub(crate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_left: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionResumptionUpdate {
    // Changed to pub(crate)
    pub new_handle: String,
    pub resumable: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq)] // Added Serialize, PartialEq
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    // Keep this public as it's part of unified events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_content_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_token_count: Option<i32>, // Changed from candidates_token_count? Verify
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_prompt_token_count: Option<i32>, // Added if relevant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thoughts_token_count: Option<i32>, // Added if relevant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_token_count: Option<i32>,

    // Add OpenAI specific fields if needed, mark optional
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_token_details: Option<Value>, // Placeholder for OpenAI structure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_token_details: Option<Value>, // Placeholder for OpenAI structure
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerMessage {
    // Changed to pub(crate) - Gemini specific wrapper
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>, // Uses common struct
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_complete: Option<BidiGenerateContentSetupComplete>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_content: Option<BidiGenerateContentServerContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<BidiGenerateContentToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_cancellation: Option<BidiGenerateContentToolCallCancellation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub go_away: Option<GoAway>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resumption_update: Option<SessionResumptionUpdate>,
    // Add other Gemini specific fields if needed
}

// --- Common Configuration Structs ---

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)] // Added PartialEq
#[serde(rename_all = "camelCase")]
pub struct RealtimeInputConfig {
    // Keep public if used in builder
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_activity_detection: Option<AutomaticActivityDetection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_handling: Option<ActivityHandling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_coverage: Option<TurnCoverage>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)] // Added PartialEq
#[serde(rename_all = "camelCase")]
pub struct AutomaticActivityDetection {
    // Keep public if used in builder
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_of_speech_sensitivity: Option<StartSensitivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_of_speech_sensitivity: Option<EndSensitivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<i32>,
}

// --- Session Resumption / Compression (Keep if needed, mark pub(crate) if Gemini only) ---

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionResumptionConfig {
    // Changed to pub(crate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextWindowCompressionConfig {
    // Changed to pub(crate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sliding_window: Option<SlidingWindow>,
    pub trigger_tokens: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SlidingWindow {
    // Changed to pub(crate)
    pub target_tokens: i64,
}
