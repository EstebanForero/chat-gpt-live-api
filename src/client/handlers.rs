use crate::types::{BidiGenerateContentServerContent, UsageMetadata};
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ServerContentContext {
    pub content: BidiGenerateContentServerContent,
}

#[derive(Debug, Clone)]
pub struct UsageMetadataContext {
    pub metadata: UsageMetadata,
}

pub trait EventHandlerSimple<Args, S_CLIENT: Clone + Send + Sync + 'static>:
    Send + Sync + 'static
{
    fn call(
        &self,
        args: Args,
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
}

impl<F, Fut, Args, S_CLIENT> EventHandlerSimple<Args, S_CLIENT> for F
where
    F: Fn(Args, Arc<S_CLIENT>) -> Fut + Send + Sync + 'static,
    S_CLIENT: Clone + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
    Args: Send + 'static,
{
    fn call(
        &self,
        args: Args,
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        Box::pin(self(args, state))
    }
}

pub trait ToolHandler<S_CLIENT: Clone + Send + Sync + 'static>: Send + Sync + 'static {
    fn call(
        &self,
        args: Option<serde_json::Value>,
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send + 'static>>;
}

impl<F, Fut, S_CLIENT> ToolHandler<S_CLIENT> for F
where
    F: Fn(Option<serde_json::Value>, Arc<S_CLIENT>) -> Fut + Send + Sync + 'static,
    S_CLIENT: Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<serde_json::Value, String>> + Send + 'static,
{
    fn call(
        &self,
        args: Option<serde_json::Value>,
        state: Arc<S_CLIENT>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send + 'static>> {
        Box::pin(self(args, state))
    }
}

pub(crate) struct Handlers<S_CLIENT: Clone + Send + Sync + 'static> {
    pub(crate) on_server_content:
        Option<Arc<dyn EventHandlerSimple<ServerContentContext, S_CLIENT>>>,
    pub(crate) on_usage_metadata:
        Option<Arc<dyn EventHandlerSimple<UsageMetadataContext, S_CLIENT>>>,
    pub(crate) tool_handlers: HashMap<String, Arc<dyn ToolHandler<S_CLIENT>>>,
    _phantom_s: PhantomData<S_CLIENT>,
}

impl<S_CLIENT: Clone + Send + Sync + 'static> Default for Handlers<S_CLIENT> {
    fn default() -> Self {
        Self {
            on_server_content: None,
            on_usage_metadata: None,
            tool_handlers: HashMap::new(),
            _phantom_s: PhantomData,
        }
    }
}
