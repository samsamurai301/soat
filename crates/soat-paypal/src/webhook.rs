//! `POST /api/payments/paypal/webhook`.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use soat_payments::http::Method;
use soat_tenancy::Scope;
use zapline::db::Db;
use zapline::http::HeaderMap;
use zapline::prelude::*;

use crate::{order_update, Inner};

#[derive(Clone)]
pub(crate) struct Shared(pub Arc<Inner>);

pub(crate) fn mount(app: App) -> App {
    app.mount((receive,))
}

/// The shop a PayPal order names; PayPal acts, not a user.
struct EventScope(String);

impl Scope for EventScope {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// Asks PayPal whether it sent this event to this server's webhook.
async fn verified(inner: &Inner, headers: &HeaderMap, event: &Value) -> Result<bool> {
    if inner.config.webhook_id.is_empty() {
        return Ok(false);
    }
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let check = json!({
        "auth_algo": h("paypal-auth-algo"),
        "cert_url": h("paypal-cert-url"),
        "transmission_id": h("paypal-transmission-id"),
        "transmission_sig": h("paypal-transmission-sig"),
        "transmission_time": h("paypal-transmission-time"),
        "webhook_id": inner.config.webhook_id,
        "webhook_event": event,
    });
    let answer = inner
        .call(
            Method::POST,
            "/v1/notifications/verify-webhook-signature",
            Some(&check),
            None,
            None,
        )
        .await?;
    Ok(answer["verification_status"] == "SUCCESS")
}

/// The PayPal order an event is about.
fn order_id(event: &Value) -> Option<&str> {
    let kind = event["event_type"].as_str().unwrap_or_default();
    let resource = &event["resource"];
    let id = if kind.starts_with("CHECKOUT.ORDER.") {
        resource["id"].as_str()
    } else if kind.starts_with("PAYMENT.CAPTURE.") || kind.starts_with("PAYMENT.AUTHORIZATION.") {
        resource["supplementary_data"]["related_ids"]["order_id"].as_str()
    } else {
        None
    };
    id.filter(|id| id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// PayPal's events about orders, captures and authorizations. Each is
/// verified with PayPal (anything else is a 400); the server then reads
/// the order from PayPal, captures or authorizes it once the buyer
/// approved it, and moves the payment. Events about payments this server
/// did not start are acknowledged and ignored.
#[post("/api/payments/paypal/webhook", tag = "payments")]
async fn receive(
    db: Db,
    shared: State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode> {
    let inner = &shared.0 .0;
    let event: Value = serde_json::from_slice(&body)
        .map_err(|_| Error::bad_request("not JSON").with_code("bad_event"))?;
    if !verified(inner, &headers, &event).await? {
        return Err(
            Error::bad_request("PayPal did not confirm this event").with_code("bad_signature")
        );
    }
    let Some(id) = order_id(&event) else {
        return Ok(StatusCode::OK);
    };
    let order = inner.order(id).await?;
    let custom = order["purchase_units"][0]["custom_id"]
        .as_str()
        .unwrap_or_default();
    let Some((tenant, payment_id)) = custom.split_once(':') else {
        return Ok(StatusCode::OK);
    };
    let scope = EventScope(tenant.to_owned());
    let Ok(mut tx) = scope.begin(&db).await else {
        return Ok(StatusCode::OK);
    };
    let payment = soat_payments::get(&mut tx, &scope, payment_id).await?;
    tx.commit().await?;
    let Some(payment) = payment else {
        return Ok(StatusCode::OK);
    };
    match payment.provider_ref.as_deref() {
        _ if payment.provider != "paypal" => return Ok(StatusCode::OK),
        None => return Err(Error::conflict("the payment is still starting").with_code("not_ready")),
        Some(r) if r != id => return Ok(StatusCode::OK),
        Some(_) => {}
    }
    let order = if order["status"] == "APPROVED" {
        inner.complete(&order, &payment.id).await?
    } else {
        order
    };
    soat_payments::apply(&db, &scope, &payment.id, &order_update(&order)).await?;
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_of_events() {
        let e = json!({ "event_type": "CHECKOUT.ORDER.APPROVED", "resource": { "id": "5O190127TN364715T" } });
        assert_eq!(order_id(&e), Some("5O190127TN364715T"));
        let e = json!({ "event_type": "PAYMENT.CAPTURE.COMPLETED", "resource": { "id": "C1",
            "supplementary_data": { "related_ids": { "order_id": "O1" } } } });
        assert_eq!(order_id(&e), Some("O1"));
        let e = json!({ "event_type": "CHECKOUT.ORDER.APPROVED", "resource": { "id": "../x" } });
        assert_eq!(order_id(&e), None);
        assert_eq!(
            order_id(&json!({ "event_type": "BILLING.PLAN.CREATED" })),
            None
        );
    }
}
