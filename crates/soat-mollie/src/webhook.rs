//! `POST /api/payments/mollie/webhook`.

use std::sync::Arc;

use bytes::Bytes;
use soat_payments::http::{form_field, Method};
use soat_tenancy::Scope;
use zapline::db::Db;
use zapline::prelude::*;

use crate::{call, payment_update, MollieConfig};

#[derive(Clone)]
pub(crate) struct Config(pub Arc<MollieConfig>);

pub(crate) fn mount(app: App) -> App {
    app.mount((receive,))
}

/// The shop a Mollie payment names; Mollie acts, not a user.
struct EventScope(String);

impl Scope for EventScope {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// Mollie's call that a payment changed: the body holds only its id
/// (`id=tr_...`), so the server asks Mollie for the payment itself.
/// Payments this server did not start are acknowledged and ignored; one
/// that arrives before its payment was recorded is a 409, so Mollie
/// tries again.
#[post("/api/payments/mollie/webhook", tag = "payments")]
async fn receive(db: Db, config: State<Config>, body: Bytes) -> Result<StatusCode> {
    let Some(id) = form_field(&body, "id").filter(|id| {
        id.starts_with("tr_")
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) else {
        return Ok(StatusCode::OK);
    };
    let config = &config.0 .0;
    let p = match call(
        config,
        Method::GET,
        &format!("/v2/payments/{id}"),
        None,
        None,
    )
    .await
    {
        Ok(p) => p,
        // Not Mollie's, or not ours to see.
        Err(e) if e.status() == StatusCode::UNPROCESSABLE_ENTITY => return Ok(StatusCode::OK),
        Err(e) => return Err(e),
    };
    let (Some(tenant), Some(payment_id)) = (
        p["metadata"]["soat_tenant"].as_str(),
        p["metadata"]["soat_payment"].as_str(),
    ) else {
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
        _ if payment.provider != "mollie" => return Ok(StatusCode::OK),
        None => return Err(Error::conflict("the payment is still starting").with_code("not_ready")),
        Some(r) if r != id => return Ok(StatusCode::OK),
        Some(_) => {}
    }
    soat_payments::apply(&db, &scope, &payment.id, &payment_update(&p)).await?;
    Ok(StatusCode::OK)
}
