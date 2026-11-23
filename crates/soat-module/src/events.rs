//! Domain events as modules see them: what happened, and who reacts.
//!
//! A module publishes an event in the same transaction as the change
//! (`soat_events::publish`), so an event exists exactly when the change
//! does. Subscribers, declared by modules, receive each event at least once,
//! retried with backoff. Handlers must therefore be idempotent.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use zapline::Resources;

/// Something that happened in a shop.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    /// `evt_...`, time-ordered.
    pub id: String,
    pub tenant_id: String,
    /// `resource.verb`, e.g. `product.created`.
    pub kind: String,
    /// The resource as the API shows it, after the change.
    pub payload: serde_json::Value,
}

/// What a subscriber's handler returns.
pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = zapline::Result<()>> + Send + 'a>>;

/// Reacts to events. An error, or a panic, retries the event later.
pub trait Subscriber: Send + Sync + 'static {
    /// Unique and stable: deliveries are tracked by it.
    fn name(&self) -> &'static str;

    /// Whether this subscriber wants events of `kind` (default: all).
    fn wants(&self, kind: &str) -> bool {
        let _ = kind;
        true
    }

    /// Handles one event. `resources` holds the app's database pool and
    /// state.
    fn handle<'a>(&'a self, event: &'a Event, resources: &'a Resources) -> HandlerFuture<'a>;
}

/// Every subscriber of an app's modules, available as `State<Subscribers>`.
#[derive(Clone, Default)]
pub struct Subscribers(pub Arc<Vec<Arc<dyn Subscriber>>>);

impl Subscribers {
    pub fn all(&self) -> &[Arc<dyn Subscriber>] {
        &self.0
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Subscriber>> {
        self.0.iter().find(|s| s.name() == name)
    }
}
