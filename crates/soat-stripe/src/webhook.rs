//! `POST /api/payments/stripe/webhook`.

use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::Value;
use soat_payments::{PaymentStatus, ProviderUpdate};
use soat_tenancy::Scope;
use zapline::db::Db;
use zapline::http::HeaderMap;
use zapline::prelude::*;

use crate::intent_update;
use crate::signature::verify;

#[derive(Clone)]
pub(crate) struct WebhookSecret(pub String);

pub(crate) fn mount(app: App) -> App {
    app.mount((receive,))
}

/// The shop a signed event names; Stripe acts, not a user.
struct EventScope(String);

impl Scope for EventScope {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// Stripe's events about PaymentIntents. Authenticated by the
/// `Stripe-Signature` header instead of a permission: anything unsigned,
/// or signed more than five minutes ago, is a 400. Events about payments
/// this server did not start are acknowledged and ignored; one that
/// arrives before its payment was recorded is a 409, so Stripe retries.
#[post("/api/payments/stripe/webhook", tag = "payments")]
async fn receive(
    db: Db,
    secret: State<WebhookSecret>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode> {
    let header = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if secret.0 .0.is_empty() || !verify(&secret.0 .0, header, &body, now) {
        return Err(Error::bad_request("bad Stripe signature").with_code("bad_signature"));
    }
    let event: Value = serde_json::from_slice(&body)
        .map_err(|_| Error::bad_request("not JSON").with_code("bad_event"))?;
    let kind = event["type"].as_str().unwrap_or_default();
    let pi = &event["data"]["object"];
    if !kind.starts_with("payment_intent.") || pi["object"] != "payment_intent" {
        return Ok(StatusCode::NO_CONTENT);
    }
    let (Some(tenant), Some(payment_id), Some(intent)) = (
        pi["metadata"]["soat_tenant"].as_str(),
        pi["metadata"]["soat_payment"].as_str(),
        pi["id"].as_str(),
    ) else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let scope = EventScope(tenant.to_owned());
    let mut tx = match scope.begin(&db).await {
        Ok(tx) => tx,
        Err(_) => return Ok(StatusCode::NO_CONTENT),
    };
    let payment = soat_payments::get(&mut tx, &scope, payment_id).await?;
    tx.commit().await?;
    let Some(payment) = payment else {
        return Ok(StatusCode::NO_CONTENT);
    };
    // Metadata is only a hint: the intent must be the one this payment
    // started, so no other account's events can move it.
    match payment.provider_ref.as_deref() {
        _ if payment.provider != "stripe" => return Ok(StatusCode::NO_CONTENT),
        None => return Err(Error::conflict("the payment is still starting").with_code("not_ready")),
        Some(r) if r != intent => return Ok(StatusCode::NO_CONTENT),
        Some(_) => {}
    }
    let update = if kind == "payment_intent.payment_failed" {
        ProviderUpdate {
            status: PaymentStatus::Failed,
            ..intent_update(pi)
        }
    } else {
        intent_update(pi)
    };
    soat_payments::apply(&db, &scope, &payment.id, &update).await?;
    Ok(StatusCode::NO_CONTENT)
}
