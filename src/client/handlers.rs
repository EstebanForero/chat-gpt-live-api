use super::backend::AppState;
use crate::types::UsageMetadata; // Keep needed types
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData; // Added PhantomData
use std::pin::Pin;
use std::sync::Arc; // Use AppState trait

// --- Context Structs ---
// Simplified context for content updates
#[derive(Debug, Clone)]
pub struct ServerContentContext {
    pub text: Option<String>,
    pub audio: Option<Vec<i16>>, // Decoded audio samples
    pub is_done: bool,           // Indicates if this specific text/audio part is final
}

#[derive(Debug, Clone)]
pub struct UsageMetadataContext {
    pub metadata: UsageMetadata,
}

// TODO: Add context structs for other events if needed (Transcription, Error, etc.)

// --- Handler Traits ---

// Trait for simple event handlers (like usage, content update)
// Takes Arc<dyn AppState>
pub trait EventHandlerSimple<Args, S_CLIENT: ?Sized + AppState + Send + Sync + 'static>:
    Send + Sync + 'static
{
    fn call(
        &self,
        args: Args,
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
}

// Blanket impl for functions matching the EventHandlerSimple signature
impl<F, Fut, Args, S_CLIENT> EventHandlerSimple<Args, S_CLIENT> for F
where
    F: Fn(Args, Arc<S_CLIENT>) -> Fut + Send + Sync + 'static,
    S_CLIENT: ?Sized + AppState + Send + Sync + 'static,
    Args: Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn call(
        &self,
        args: Args,
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        Box::pin(self(args, state))
    }
}

// Trait for tool call handlers
// Takes Arc<dyn AppState>
pub trait ToolHandler<S_CLIENT: ?Sized + AppState + Send + Sync + 'static>:
    Send + Sync + 'static
{
    fn call(
        &self,
        args: Option<Value>, // Arguments received from the model
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>>; // Return Result<Value, ErrorString>
}

// Blanket impl for functions matching the ToolHandler signature
impl<F, Fut, S_CLIENT> ToolHandler<S_CLIENT> for F
where
    F: Fn(Option<Value>, Arc<S_CLIENT>) -> Fut + Send + Sync + 'static,
    S_CLIENT: ?Sized + AppState + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    fn call(
        &self,
        args: Option<Value>,
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>> {
        Box::pin(self(args, state))
    }
}

// Internal Handlers struct holding type-erased handlers
#[derive(Default)] // Added default derive
pub(crate) struct Handlers<S_CLIENT: ?Sized + AppState + Send + Sync + 'static> {
    pub(crate) on_server_content:
        Option<Arc<dyn EventHandlerSimple<ServerContentContext, S_CLIENT>>>,
    pub(crate) on_usage_metadata:
        Option<Arc<dyn EventHandlerSimple<UsageMetadataContext, S_CLIENT>>>,
    // TODO: Add fields for other handlers (on_transcription, on_error etc.)
    pub(crate) tool_handlers: HashMap<String, Arc<dyn ToolHandler<S_CLIENT>>>,
    _phantom_s: PhantomData<Arc<S_CLIENT>>, // Use PhantomData correctly
}

// No longer needs Default impl as it's derived
// impl<S_CLIENT: ?Sized + AppState + Send + Sync + 'static> Default for Handlers<S_CLIENT> { ... }
