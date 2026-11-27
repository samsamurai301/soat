//! Peppol: a shop's invoices and credit notes sent over the Peppol network.
//!
//! Peppol connects buyers and sellers through certified access points: a
//! shop hands its access point a Peppol BIS Billing 3.0 document (UBL) with
//! its own Peppol id and the buyer's, and the access point finds the
//! buyer's and delivers it. A shop picks an access point adapter with its
//! credentials (sealed with the install's secret key, never shown again)
//! and its Peppol id; staff then send an issued invoice or credit note to
//! the buyer's Peppol id. Each try is kept, with the access point's
//! reference or why it failed.
//!
//! An adapter implements [`AccessPoint`] and is registered with
//! [`Peppol::with`]. The built-in `http` adapter posts the document to any
//! service that speaks SOAT's request (see `docs/e-invoices.md`), such as a
//! small bridge to the access point a shop has a contract with.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use soat_core::einvoice::{self, Format};
use soat_core::peppol::{self, ParticipantId};
use soat_core::secret::Sealer;
use soat_core::{id_kind, Permission};
use soat_http::{Bytes, Full, Method, Request};
use soat_module::Module;
use soat_tenancy::Tenant;
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(TransmissionId, TransmissionKind, "ppt");

/// Send invoices and credit notes over Peppol, and see what was sent.
pub const PEPPOL_SEND: Permission =
    Permission::new("peppol:send", "Send invoices over Peppol").for_staff();
/// Choose the shop's Peppol access point and Peppol id. Not given to staff
/// by default.
pub const PEPPOL_MANAGE: Permission =
    Permission::new("peppol:manage", "Choose the Peppol access point");

/// What an adapter call returns.
pub type AccessPointFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// What an access point is handed.
#[derive(Debug, Clone)]
pub struct Delivery<'a> {
    /// The shop's credentials for it, unsealed.
    pub credentials: &'a Value,
    /// The shop's settings for it.
    pub settings: &'a Value,
    pub sender: &'a ParticipantId,
    pub receiver: &'a ParticipantId,
    /// [`peppol::INVOICE_DOCUMENT`] or [`peppol::CREDIT_NOTE_DOCUMENT`].
    pub document_type: &'a str,
    /// [`peppol::BILLING_PROCESS`].
    pub process: &'a str,
    /// The invoice's number, e.g. `INV-2026-00001`.
    pub number: &'a str,
    /// The Peppol BIS Billing 3.0 UBL document.
    pub xml: &'a str,
}

/// A Peppol access point.
pub trait AccessPoint: Send + Sync {
    /// e.g. `http`.
    fn name(&self) -> &'static str;
    /// e.g. `Access point (HTTP)`.
    fn label(&self) -> &'static str;
    /// Checks a shop's credentials and settings before they are kept.
    fn check(&self, credentials: &Value, settings: &Value) -> Result<()>;
    /// Hands the document over; answers the access point's reference for
    /// it. An error says why, for staff to read.
    fn send<'a>(&'a self, delivery: &'a Delivery<'a>) -> AccessPointFuture<'a, String>;
}

/// The Peppol module, with the access points this install offers (the
/// built-in `http` adapter to start with).
pub struct Peppol {
    adapters: Vec<Arc<dyn AccessPoint>>,
    sealer: Option<Sealer>,
}

impl Default for Peppol {
    fn default() -> Self {
        Self::new()
    }
}

impl Peppol {
    pub fn new() -> Self {
        Self {
            adapters: vec![Arc::new(HttpAccessPoint)],
            sealer: None,
        }
    }

    /// Offers `adapter` to shops, replacing one of the same name.
    pub fn with(mut self, adapter: impl AccessPoint + 'static) -> Self {
        self.adapters.retain(|a| a.name() != adapter.name());
        self.adapters.push(Arc::new(adapter));
        self
    }

    /// Seals access point credentials with `sealer`; without one, shops
    /// cannot choose an access point.
    pub fn with_sealer(mut self, sealer: Option<Sealer>) -> Self {
        self.sealer = sealer;
        self
    }
}

impl Module for Peppol {
    fn name(&self) -> &'static str {
        "peppol"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "invoices"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[PEPPOL_SEND, PEPPOL_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.state(AccessPoints {
            list: Arc::new(self.adapters.clone()),
            sealer: self.sealer.clone(),
        })
        .mount((
            peppol_access_point_kinds,
            get_peppol_access_point,
            put_peppol_access_point,
            delete_peppol_access_point,
            send_over_peppol,
            peppol_transmissions,
        ))
    }
}

/// The adapters of this install and the key that seals credentials.
#[derive(Clone)]
struct AccessPoints {
    list: Arc<Vec<Arc<dyn AccessPoint>>>,
    sealer: Option<Sealer>,
}

impl AccessPoints {
    fn get(&self, name: &str) -> Option<&Arc<dyn AccessPoint>> {
        self.list.iter().find(|a| a.name() == name)
    }

    fn sealer(&self) -> Result<&Sealer> {
        self.sealer.as_ref().ok_or_else(|| {
            Error::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "this install keeps no secrets: set SOAT_SECRET_KEY",
            )
            .with_code("secrets_unavailable")
        })
    }
}

/// The built-in adapter: POSTs the document as JSON to the shop's `url`,
/// signed like webhooks (`soat-signature`) with its `secret`, and takes
/// the `id` of a 2xx answer as the reference.
pub struct HttpAccessPoint;

/// `https://…`, or plain HTTP on this machine for tests and development.
fn is_service_url(url: &str) -> bool {
    let local = ["http://localhost", "http://127.0.0.1"].iter().any(|p| {
        url.strip_prefix(p)
            .is_some_and(|r| r.is_empty() || r.starts_with([':', '/']))
    });
    let https = url
        .strip_prefix("https://")
        .is_some_and(|r| !r.is_empty() && !r.starts_with('/'));
    (https || local) && url.len() <= 500 && !url.chars().any(char::is_whitespace)
}

impl AccessPoint for HttpAccessPoint {
    fn name(&self) -> &'static str {
        "http"
    }

    fn label(&self) -> &'static str {
        "Access point (HTTP)"
    }

    fn check(&self, credentials: &Value, settings: &Value) -> Result<()> {
        if !is_service_url(settings["url"].as_str().unwrap_or_default()) {
            return Err(
                Error::unprocessable("settings.url is the service's address: https://…")
                    .with_code("invalid_settings"),
            );
        }
        if credentials["secret"].as_str().unwrap_or_default().len() < 16 {
            return Err(Error::unprocessable(
                "credentials.secret signs the requests: at least 16 characters",
            )
            .with_code("invalid_credentials"));
        }
        Ok(())
    }

    fn send<'a>(&'a self, d: &'a Delivery<'a>) -> AccessPointFuture<'a, String> {
        Box::pin(async move {
            let url = d.settings["url"].as_str().unwrap_or_default();
            let secret = d.credentials["secret"].as_str().unwrap_or_default();
            let body = json!({
                "sender": d.sender.full(),
                "receiver": d.receiver.full(),
                "document_type": format!("{}::{}", peppol::DOCUMENT_SCHEME, d.document_type),
                "process": format!("{}::{}", peppol::PROCESS_SCHEME, d.process),
                "number": d.number,
                "document": d.xml,
            })
            .to_string();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |t| t.as_secs());
            let request = Request::builder()
                .method(Method::POST)
                .uri(url)
                .header("content-type", "application/json")
                .header(
                    "soat-signature",
                    soat_webhooks::sign(secret, now, body.as_bytes()),
                )
                .body(Full::new(Bytes::from(body)))
                .map_err(|e| Error::unprocessable(e.to_string()))?;
            let answer = soat_http::send("Access point", request).await?;
            let said = answer.json();
            if !answer.status.is_success() {
                let why = said["error"]
                    .as_str()
                    .or_else(|| said["message"].as_str())
                    .map(|m| format!(": {}", m.chars().take(300).collect::<String>()))
                    .unwrap_or_default();
                return Err(Error::service_unavailable(format!(
                    "Access point: answered {}{why}",
                    answer.status.as_u16()
                )));
            }
            said["id"]
                .as_str()
                .filter(|id| !id.trim().is_empty() && id.len() <= 200)
                .map(str::to_owned)
                .ok_or_else(|| Error::service_unavailable("Access point: the answer has no id"))
        })
    }
}

/// An access point adapter this install offers.
#[model]
pub struct AccessPointKind {
    pub name: String,
    pub label: String,
}

/// The shop's access point.
#[model]
pub struct ShopAccessPoint {
    /// The adapter, e.g. `http`.
    pub provider: String,
    /// The shop's Peppol id, e.g. `0208:0123456749`.
    pub sender_id: String,
    /// Its settings, e.g. `{"url": "https://ap.example.com/send"}`.
    pub settings: Value,
    pub created_at: String,
}

/// Body of `PUT /api/peppol/access-point`.
#[model]
pub struct SetAccessPoint {
    pub provider: String,
    /// The shop's Peppol id: scheme and id, e.g. `0208:0123456749`.
    #[validate(length(max = 100))]
    pub sender_id: String,
    #[serde(default)]
    pub settings: Value,
    /// Kept sealed, never shown again; for `http`, `{"secret": "…"}`.
    pub credentials: Value,
}

/// Body of `POST /api/invoices/{id}/peppol`.
#[model]
pub struct SendOverPeppol {
    /// The buyer's Peppol id, e.g. `0208:0123456749` or `9930:DE123456789`.
    #[validate(length(max = 100))]
    pub receiver_id: String,
    /// BT-10, the buyer's reference; default: the order number.
    #[validate(length(max = 200))]
    pub buyer_reference: Option<String>,
}

/// One try at sending an invoice or credit note over Peppol.
#[model]
#[derive(sqlx::FromRow)]
pub struct Transmission {
    /// e.g. `ppt_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub invoice_id: String,
    pub invoice_number: String,
    /// The adapter it went through.
    pub provider: String,
    pub sender_id: String,
    pub receiver_id: String,
    pub document_type: String,
    /// `sent` (the access point took it) or `failed`.
    pub status: String,
    /// The access point's id for it, once sent.
    pub reference: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    /// RFC 3339, UTC.
    pub created_at: String,
}

const TRANSMISSION: &str = "id, invoice_id, invoice_number, provider, sender_id, receiver_id, \
     document_type, status, reference, error, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

fn participant(field: &str, s: &str) -> Result<ParticipantId> {
    ParticipantId::parse(s)
        .map_err(|e| Error::unprocessable(format!("{field}: {e}")).with_code("invalid_peppol_id"))
}

async fn current(conn: &mut PgConnection, tenant: &Tenant) -> Result<Option<ShopAccessPoint>> {
    let row: Option<(String, String, SqlJson<Value>, String)> = sqlx::query_as(
        "SELECT provider, sender_id, settings, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
         FROM peppol_access_points WHERE tenant_id = $1",
    )
    .bind(tenant.id())
    .fetch_optional(conn)
    .await?;
    Ok(row.map(
        |(provider, sender_id, settings, created_at)| ShopAccessPoint {
            provider,
            sender_id,
            settings: settings.0,
            created_at,
        },
    ))
}

/// The Peppol access points this install offers (`peppol:send`).
#[get("/api/peppol/access-points", tag = "peppol")]
async fn peppol_access_point_kinds(
    db: Db,
    tenant: Tenant,
    adapters: State<AccessPoints>,
) -> Result<Json<Vec<AccessPointKind>>> {
    let tx = tenant.begin_for(&db, &PEPPOL_SEND).await?;
    tx.commit().await?;
    Ok(Json(
        adapters
            .list
            .iter()
            .map(|a| AccessPointKind {
                name: a.name().to_owned(),
                label: a.label().to_owned(),
            })
            .collect(),
    ))
}

/// The shop's Peppol access point and Peppol id (`peppol:send`); 404 when
/// it has none.
#[get("/api/peppol/access-point", tag = "peppol")]
async fn get_peppol_access_point(db: Db, tenant: Tenant) -> Result<Json<ShopAccessPoint>> {
    let mut tx = tenant.begin_for(&db, &PEPPOL_SEND).await?;
    let found = current(&mut tx, &tenant).await?;
    tx.commit().await?;
    found.map(Json).ok_or_else(Error::not_found)
}

/// Choose the access point the shop sends through, and its Peppol id
/// (`peppol:manage`), replacing any before. The credentials are sealed and
/// never shown again. Publishes `peppol.access_point_set`, without them.
#[put("/api/peppol/access-point", tag = "peppol")]
async fn put_peppol_access_point(
    db: Db,
    tenant: Tenant,
    adapters: State<AccessPoints>,
    Valid(Json(b)): Valid<Json<SetAccessPoint>>,
) -> Result<Json<ShopAccessPoint>> {
    let adapter = adapters.get(&b.provider).ok_or_else(|| {
        Error::unprocessable(format!("no access point {:?}", b.provider))
            .with_code("unknown_provider")
    })?;
    let sender = participant("sender_id", &b.sender_id)?;
    let settings = if b.settings.is_null() {
        json!({})
    } else {
        b.settings
    };
    adapter.check(&b.credentials, &settings)?;
    let sealed = adapters
        .sealer()?
        .seal(b.credentials.to_string().as_bytes(), tenant.id());
    let mut tx = tenant.begin_for(&db, &PEPPOL_MANAGE).await?;
    sqlx::query(
        "INSERT INTO peppol_access_points (tenant_id, provider, sender_id, settings, credentials) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id) DO UPDATE SET \
         provider = EXCLUDED.provider, sender_id = EXCLUDED.sender_id, \
         settings = EXCLUDED.settings, credentials = EXCLUDED.credentials, created_at = now()",
    )
    .bind(tenant.id())
    .bind(&b.provider)
    .bind(sender.to_string())
    .bind(SqlJson(&settings))
    .bind(sealed)
    .execute(&mut *tx)
    .await?;
    let saved = current(&mut tx, &tenant)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant
        .publish(&mut tx, "peppol.access_point_set", &saved)
        .await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// Stop sending over Peppol (`peppol:manage`); what was sent stays listed.
/// Publishes `peppol.access_point_removed`.
#[delete("/api/peppol/access-point", tag = "peppol", status = 204)]
async fn delete_peppol_access_point(db: Db, tenant: Tenant) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &PEPPOL_MANAGE).await?;
    let gone: Option<(String,)> =
        sqlx::query_as("DELETE FROM peppol_access_points WHERE tenant_id = $1 RETURNING provider")
            .bind(tenant.id())
            .fetch_optional(&mut *tx)
            .await?;
    let Some((provider,)) = gone else {
        return Err(Error::not_found());
    };
    tenant
        .publish(
            &mut tx,
            "peppol.access_point_removed",
            &json!({ "provider": provider }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Send an issued invoice or credit note to the buyer's Peppol id
/// (`peppol:send`), as Peppol BIS Billing 3.0 through the shop's access
/// point. The shop's and the buyer's Peppol ids become the document's
/// electronic addresses. 409 `no_access_point` before one is chosen, 409
/// `already_sent` when the access point took it for that buyer already,
/// 422 `not_ready` naming the rules a receiver would reject it by (see
/// `/e-invoice/check?format=peppol`). Each try is kept: 201 with the
/// access point's reference, or 502 `access_point_failed` saying why.
/// Publishes `invoice.peppol_sent` or `invoice.peppol_failed`.
#[post("/api/invoices/{id}/peppol", tag = "peppol", status = 201)]
async fn send_over_peppol(
    db: Db,
    tenant: Tenant,
    adapters: State<AccessPoints>,
    Path(id): Path<String>,
    Valid(Json(b)): Valid<Json<SendOverPeppol>>,
) -> Result<(StatusCode, Json<Transmission>)> {
    let receiver = participant("receiver_id", &b.receiver_id)?;

    // Gather what to send, then let go of the database while the access
    // point is called.
    let mut tx = tenant.begin_for(&db, &PEPPOL_SEND).await?;
    let (inv, mut doc) =
        soat_invoices::e_invoice(&mut tx, &tenant, &id, b.buyer_reference.as_deref())
            .await?
            .ok_or_else(Error::not_found)?;
    let row: Option<(String, String, SqlJson<Value>, Vec<u8>)> = sqlx::query_as(
        "SELECT provider, sender_id, settings, credentials FROM peppol_access_points \
         WHERE tenant_id = $1",
    )
    .bind(tenant.id())
    .fetch_optional(&mut *tx)
    .await?;
    let sent: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM peppol_transmissions WHERE tenant_id = $1 AND invoice_id = $2 \
         AND receiver_id = $3 AND status = 'sent'",
    )
    .bind(tenant.id())
    .bind(&inv.id)
    .bind(receiver.to_string())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    let Some((provider, sender_id, settings, sealed)) = row else {
        return Err(Error::conflict(
            "choose the shop's Peppol access point first: PUT /api/peppol/access-point",
        )
        .with_code("no_access_point"));
    };
    if let Some((earlier,)) = sent {
        return Err(Error::conflict(format!(
            "{} went to {receiver} already ({earlier})",
            inv.number
        ))
        .with_code("already_sent"));
    }
    let sender = participant("sender_id", &sender_id)?;
    doc.seller.endpoint = Some(sender.clone());
    doc.buyer.endpoint = Some(receiver.clone());
    let problems = einvoice::check(&doc, Format::Peppol);
    if !problems.is_empty() {
        let list: Vec<String> = problems
            .iter()
            .map(|p| format!("{}: {}", p.rule, p.message))
            .collect();
        return Err(Error::unprocessable(format!(
            "a receiver would reject {}: {}",
            inv.number,
            list.join(" ")
        ))
        .with_code("not_ready"));
    }
    let xml = einvoice::xml(&doc, Format::Peppol);
    let document_type = peppol::document_type(doc.credit_note);

    let outcome = match adapters.get(&provider) {
        None => Err(format!("this install has no access point {provider:?}")),
        Some(adapter) => {
            let credentials = adapters
                .sealer
                .as_ref()
                .and_then(|s| s.open(&sealed, tenant.id()))
                .and_then(|c| serde_json::from_slice::<Value>(&c).ok());
            match credentials {
                None => Err("the access point's credentials cannot be opened".to_owned()),
                Some(credentials) => {
                    let delivery = Delivery {
                        credentials: &credentials,
                        settings: &settings.0,
                        sender: &sender,
                        receiver: &receiver,
                        document_type,
                        process: peppol::BILLING_PROCESS,
                        number: &inv.number,
                        xml: &xml,
                    };
                    adapter.send(&delivery).await.map_err(|e| e.to_string())
                }
            }
        }
    };

    let mut tx = tenant.begin_for(&db, &PEPPOL_SEND).await?;
    let (status, reference, error) = match &outcome {
        Ok(reference) => ("sent", Some(reference.as_str()), None),
        Err(e) => ("failed", None, Some(e.as_str())),
    };
    let saved: Option<Transmission> = sqlx::query_as(&format!(
        "INSERT INTO peppol_transmissions (id, tenant_id, invoice_id, invoice_number, provider, \
         sender_id, receiver_id, document_type, status, reference, error) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (tenant_id, invoice_id, receiver_id) WHERE status = 'sent' DO NOTHING \
         RETURNING {TRANSMISSION}"
    ))
    .bind(TransmissionId::new().to_string())
    .bind(tenant.id())
    .bind(&inv.id)
    .bind(&inv.number)
    .bind(&provider)
    .bind(sender.to_string())
    .bind(receiver.to_string())
    .bind(document_type)
    .bind(status)
    .bind(reference)
    .bind(error)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(saved) = saved else {
        // Another request sent it meanwhile.
        return Err(
            Error::conflict(format!("{} went to {receiver} already", inv.number))
                .with_code("already_sent"),
        );
    };
    let event = if outcome.is_ok() {
        "invoice.peppol_sent"
    } else {
        "invoice.peppol_failed"
    };
    tenant.publish(&mut tx, event, &saved).await?;
    tx.commit().await?;
    match outcome {
        Ok(_) => Ok((StatusCode::CREATED, Json(saved))),
        Err(e) => Err(Error::new(
            StatusCode::BAD_GATEWAY,
            format!("{} was not sent ({}): {e}", inv.number, saved.id),
        )
        .with_code("access_point_failed")),
    }
}

/// Every try at sending an invoice or credit note over Peppol, newest
/// first (`peppol:send`).
#[get("/api/invoices/{id}/peppol", tag = "peppol")]
async fn peppol_transmissions(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Vec<Transmission>>> {
    let mut tx = tenant.begin_for(&db, &PEPPOL_SEND).await?;
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM invoices WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
    if exists.is_none() {
        return Err(Error::not_found());
    }
    let list: Vec<Transmission> = sqlx::query_as(&format!(
        "SELECT {TRANSMISSION} FROM peppol_transmissions WHERE tenant_id = $1 \
         AND invoice_id = $2 ORDER BY created_at DESC, id DESC"
    ))
    .bind(tenant.id())
    .bind(&id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(list))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_http_adapter_checks_its_settings() {
        let a = HttpAccessPoint;
        let secret = json!({ "secret": "a secret of some length" });
        assert!(a
            .check(&secret, &json!({ "url": "https://ap.example.com/send" }))
            .is_ok());
        for url in ["http://ap.example.com/send", "", "https:///x"] {
            assert!(a.check(&secret, &json!({ "url": url })).is_err(), "{url}");
        }
        assert!(a
            .check(
                &json!({ "secret": "short" }),
                &json!({ "url": "https://ap.example.com" })
            )
            .is_err());
    }
}
