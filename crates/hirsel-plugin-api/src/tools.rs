//! Agent tools contributed by a plugin.

use std::{future::Future, pin::Pin, sync::Arc};

use serde_json::Value;

use crate::ctx::PluginCtx;

/// The boxed future a tool handler returns. Boxing keeps [`PluginToolHandler`]
/// object-safe, which is what lets the host hold a heterogeneous tool table.
pub type PluginToolFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;

/// A plugin tool body. Implemented for any
/// `Fn(PluginCtx, Value) -> PluginToolFuture`, so the common case is a closure
/// returning `Box::pin(async move { .. })`.
pub trait PluginToolHandler: Send + Sync {
    fn call(&self, ctx: PluginCtx, args: Value) -> PluginToolFuture;
}

impl<F> PluginToolHandler for F
where
    F: Fn(PluginCtx, Value) -> PluginToolFuture + Send + Sync,
{
    fn call(&self, ctx: PluginCtx, args: Value) -> PluginToolFuture {
        (self)(ctx, args)
    }
}

/// One tool the agent can call. The host registers it as
/// `plugin__<id_with_underscores>__<name>` and enforces a 120s timeout on the
/// handler.
#[derive(Clone)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub handler: Arc<dyn PluginToolHandler>,
}

impl PluginTool {
    /// The ergonomic constructor: `handler` is any
    /// `Fn(PluginCtx, Value) -> impl Future<Output = Result<Value, String>>`,
    /// so an `async move` block works with no boxing at the call site.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(PluginCtx, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        Self::from_handler(name, description, input_schema, FnHandler(handler))
    }

    /// Same, for a handler that is its own type (state in a struct, say).
    pub fn from_handler(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: impl PluginToolHandler + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            handler: Arc::new(handler),
        }
    }
}

struct FnHandler<F>(F);

impl<F, Fut> PluginToolHandler for FnHandler<F>
where
    F: Fn(PluginCtx, Value) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    fn call(&self, ctx: PluginCtx, args: Value) -> PluginToolFuture {
        Box::pin((self.0)(ctx, args))
    }
}

impl std::fmt::Debug for PluginTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}
