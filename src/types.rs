use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::HashMap;

// --- Enums ---
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpeechLanguageCode {
    #[serde(rename = "en-US")]
    EnglishUS,
    #[serde(rename = "en-GB")]
    EnglishGB,
    #[serde(rename = "en-AU")]
    EnglishAU,
    #[serde(rename = "en-IN")]
    EnglishIN,
    #[serde(rename = "es-ES")]
    SpanishES,
    #[serde(rename = "es-US")]
    SpanishUS,
    #[serde(rename = "de-DE")]
    GermanDE,
    #[serde(rename = "fr-FR")]
    FrenchFR,
    #[serde(rename = "fr-CA")]
    FrenchCA,
    #[serde(rename = "hi-IN")]
    HindiIN,
    #[serde(rename = "pt-BR")]
    PortugueseBR,
    #[serde(rename = "ar-XA")]
    ArabicXA,
    #[serde(rename = "id-ID")]
    IndonesianID,
    #[serde(rename = "it-IT")]
    ItalianIT,
    #[serde(rename = "ja-JP")]
    JapaneseJP,
    #[serde(rename = "tr-TR")]
    TurkishTR,
    #[serde(rename = "vi-VN")]
    VietnameseVN,
    #[serde(rename = "bn-IN")]
    BengaliIN,
    #[serde(rename = "gu-IN")]
    GujaratiIN,
    #[serde(rename = "kn-IN")]
    KannadaIN,
    #[serde(rename = "ml-IN")]
    MalayalamIN,
    #[serde(rename = "mr-IN")]
    MarathiIN,
    #[serde(rename = "ta-IN")]
    TamilIN,
    #[serde(rename = "te-IN")]
    TeluguIN,
    #[serde(rename = "nl-NL")]
    DutchNL,
    #[serde(rename = "ko-KR")]
    KoreanKR,
    #[serde(rename = "cmn-CN")]
    MandarinCN,
    #[serde(rename = "pl-PL")]
    PolishPL,
    #[serde(rename = "ru-RU")]
    RussianRU,
    #[serde(rename = "th-TH")]
    ThaiTH,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResponseModality {
    Text,
    Audio,
    Other(String),
}
// ***** RESTORED Serialize/Deserialize for ResponseModality *****
impl Serialize for ResponseModality {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ResponseModality::Text => serializer.serialize_str("TEXT"),
            ResponseModality::Audio => serializer.serialize_str("AUDIO"),
            ResponseModality::Other(s) => serializer.serialize_str(s),
        }
    }
}
impl<'de> Deserialize<'de> for ResponseModality {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "TEXT" => Ok(ResponseModality::Text),
            "AUDIO" => Ok(ResponseModality::Audio),
            other => Ok(ResponseModality::Other(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)] // Removed Default derive
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default] // Keep default on User
    User,
    Model,
    Function,
    System,
    #[serde(untagged)]
    Other(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActivityHandling {
    ActivityHandlingUnspecified,
    StartOfActivityInterrupts,
    NoInterruption,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StartSensitivity {
    StartSensitivityUnspecified,
    StartSensitivityHigh,
    StartSensitivityLow,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EndSensitivity {
    EndSensitivityUnspecified,
    EndSensitivityHigh,
    EndSensitivityLow,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TurnCoverage {
    TurnCoverageUnspecified,
    TurnIncludesOnlyActivity,
    TurnIncludesAllInput,
}

// --- Core Structs ---

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpeechConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_code: Option<SpeechLanguageCode>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    // ***** RESTORED parts field *****
    pub parts: Vec<Part>,
    // ***** RESTORED role field *****
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    // ***** RESTORED text field *****
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    // ***** RESTORED inline_data field *****
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Blob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_code: Option<ExecutableCode>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableCode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    pub name: String,
    pub response: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)] // Keep Serialize for backend usage
#[serde(rename_all = "camelCase")]
pub struct Blob {
    // ***** RESTORED mime_type field *****
    pub mime_type: String,
    // ***** RESTORED data field *****
    pub data: String, // Base64 encoded bytes
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
    // ***** RESTORED temperature field *****
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_modalities: Option<Vec<ResponseModality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<SpeechConfig>,
    // Add other fields if needed
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub function_declarations: Vec<FunctionDeclaration>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Schema>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, Schema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionConfig {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeInputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_activity_detection: Option<AutomaticActivityDetection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_handling: Option<ActivityHandling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_coverage: Option<TurnCoverage>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticActivityDetection {
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStart {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEnd {}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_content_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_prompt_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thoughts_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_token_details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_token_details: Option<Value>,
}

// --- Gemini Specific Internal Structs (Marked pub(crate)) ---
#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentSetup {
    /* ... fields ... */
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_input_config: Option<RealtimeInputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resumption: Option<SessionResumptionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_compression: Option<ContextWindowCompressionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_audio_transcription: Option<AudioTranscriptionConfig>,
}
#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentClientContent {
    /* ... fields ... */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<Vec<Content>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<bool>,
}
#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentRealtimeInput {
    /* ... fields ... */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Blob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<Blob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_start: Option<ActivityStart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_end: Option<ActivityEnd>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_stream_end: Option<bool>,
}
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ClientMessagePayload {
    /* ... variants ... */
    Setup(BidiGenerateContentSetup),
    ClientContent(BidiGenerateContentClientContent),
    RealtimeInput(BidiGenerateContentRealtimeInput),
    ToolResponse(BidiGenerateContentToolResponse),
}
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentToolResponse {
    /* ... fields ... */
    pub function_responses: Vec<FunctionResponse>,
}
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentSetupComplete {}
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentServerContent {
    /* ... fields ... */
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
    pub model_turn: Option<Content>,
}
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentTranscription {
    /* ... fields ... */
    pub text: String,
}
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentToolCall {
    /* ... fields ... */
    pub function_calls: Vec<FunctionCall>,
}
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BidiGenerateContentToolCallCancellation {
    /* ... fields ... */
    pub ids: Vec<String>,
}
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoAway {
    /* ... fields ... */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_left: Option<String>,
}
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionResumptionUpdate {
    /* ... fields ... */
    pub new_handle: String,
    pub resumable: bool,
}
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerMessage {
    /* ... fields ... */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>,
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
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionResumptionConfig {
    /* ... fields ... */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextWindowCompressionConfig {
    /* ... fields ... */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sliding_window: Option<SlidingWindow>,
    pub trigger_tokens: i64,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SlidingWindow {
    /* ... fields ... */
    pub target_tokens: i64,
}
