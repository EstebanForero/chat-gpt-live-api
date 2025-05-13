use crate::types::UsageMetadata;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
// AppState trait removed

#[derive(Debug, Clone)]
pub struct ServerContentContext {
    pub text: Option<String>,
    pub audio: Option<Vec<i16>>,
    pub is_done: bool,
}
#[derive(Debug, Clone)]
pub struct UsageMetadataContext {
    pub metadata: UsageMetadata,
}

// SClient now uses S directly, with bounds
pub trait EventHandlerSimple<Args, S: Clone + Send + Sync + 'static>:
    Send + Sync + 'static
{
    fn call(&self, args: Args, state: Arc<S>)
    -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
}

impl<F, Fut, Args, S> EventHandlerSimple<Args, S> for F
where
    F: Fn(Args, Arc<S>) -> Fut + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
    Args: Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn call(
        &self,
        args: Args,
        state: Arc<S>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        Box::pin(self(args, state))
    }
}

// SClient now uses S directly, with bounds
pub trait ToolHandler<S: Clone + Send + Sync + 'static>: Send + Sync + 'static {
    fn call(
        &self,
        args: Option<Value>,
        state: Arc<S>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>>;
}

impl<F, Fut, S> ToolHandler<S> for F
where
    F: Fn(Option<Value>, Arc<S>) -> Fut + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    fn call(
        &self,
        args: Option<Value>,
        state: Arc<S>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>> {
        Box::pin(self(args, state))
    }
}

// Handlers is now generic over S
pub(crate) struct Handlers<S: Clone + Send + Sync + 'static> {
    pub(crate) on_server_content: Option<Arc<dyn EventHandlerSimple<ServerContentContext, S>>>,
    pub(crate) on_usage_metadata: Option<Arc<dyn EventHandlerSimple<UsageMetadataContext, S>>>,
    pub(crate) tool_handlers: HashMap<String, Arc<dyn ToolHandler<S>>>,
    _phantom_s: PhantomData<S>, // PhantomData uses S directly
}

impl<S: Clone + Send + Sync + 'static> Default for Handlers<S> {
    fn default() -> Self {
        Self {
            on_server_content: None,
            on_usage_metadata: None,
            tool_handlers: HashMap::new(),
            _phantom_s: PhantomData,
        }
    }
}
