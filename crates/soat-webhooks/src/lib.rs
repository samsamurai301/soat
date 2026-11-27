//! Outbound webhooks.
//!
//! A shop registers endpoints (`POST /api/webhooks`) for the event kinds it
//! wants, or `*`. When an event is published, the `webhooks` subscriber
//! records one delivery per matching endpoint; a sender job POSTs each as
//! JSON, signed with the endpoint's secret, and retries failures with the
//! same backoff as events. Every attempt is visible in the delivery log.
//!
//! Receivers verify the `soat-signature` header (see [`sign`]): `t=<unix
//! seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>" with the secret>`.

mod routes;
mod send;
mod sign;
mod target;

use std::sync::Arc;
use std::time::Duration;

use soat_core::{id_kind, Permission};
use soat_module::{Module, Subscriber};
use zapline::embed::Dir;
use zapline::prelude::App;
use zapline::tasks::Schedule;
use zapline::Resources;

pub use send::{backlog, send_due, send_resources};
pub use sign::{sign, verify};
pub use target::{check_target, TargetPolicy};

id_kind!(EndpointId, EndpointKind, "whk");
id_kind!(DeliveryId, DeliveryKind, "whd");

/// Register webhook endpoints and read their delivery log.
pub const WEBHOOKS_MANAGE: Permission = Permission::new(
    "webhooks:manage",
    "Register webhook endpoints and read their delivery log",
);

/// The webhooks module.
pub struct Webhooks {
    poll: Option<Duration>,
    policy: TargetPolicy,
}

impl Webhooks {
    /// Sends every second, to public HTTPS URLs only.
    pub fn new() -> Self {
        Self {
            poll: Some(Duration::from_secs(1)),
            policy: TargetPolicy::PublicHttps,
        }
    }

    /// Never sends on its own: tests call [`send_resources`].
    pub fn manual(mut self) -> Self {
        self.poll = None;
        self
    }

    /// Also allows `http://` and private or loopback addresses: for local
    /// development and tests only.
    pub fn allow_private_targets(mut self) -> Self {
        self.policy = TargetPolicy::Any;
        self
    }
}

impl Default for Webhooks {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Webhooks {
    fn name(&self) -> &'static str {
        "webhooks"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[WEBHOOKS_MANAGE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(send::Recorder)]
    }

    fn mount(&self, app: App) -> App {
        let app = app.state(self.policy).plugin(routes::mount);
        match self.poll {
            Some(every) => app.schedule(
                Schedule::every(every).name("soat-webhooks"),
                |res: Resources| async move { send_resources(&res).await.map(|_| ()) },
            ),
            None => app,
        }
    }
}
