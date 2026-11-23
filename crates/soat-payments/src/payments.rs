//! Payments and refunds.

use serde_json::Value;
use soat_core::payment::PaymentStatus;
use soat_core::{id_kind, Currency};
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::methods::PaymentMethodId;
use crate::provider::{PaymentProvider, ProviderPayment, ProviderUpdate, StartRequest};
use crate::{Providers, PAYMENTS_READ, PAYMENTS_REFUND, PAYMENTS_WRITE};

id_kind!(PaymentId, PaymentKind, "pay");
id_kind!(RefundId, RefundKind, "rfd");

/// A payment.
#[model]
pub struct Payment {
    /// e.g. `pay_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// What it pays for, e.g. an order or cart id.
    pub reference: String,
    pub method_id: Option<String>,
    pub provider: String,
    /// The provider's id for it.
    pub provider_ref: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    /// `pending`, `requires_action`, `authorized`, `paid`, `failed`,
    /// `canceled`, `partially_refunded` or `refunded`.
    pub status: String,
    pub refunded_minor: i64,
    /// What the buyer must do next, e.g. `{"type": "instructions", ...}`
    /// or a redirect.
    pub next_action: Option<Value>,
    pub failure_reason: Option<String>,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub updated_at: String,
    /// Only in the answer that started the payment, for the buyer's
    /// browser (e.g. Stripe's client secret); never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PaymentRow {
    id: String,
    reference: String,
    method_id: Option<String>,
    provider: String,
    provider_ref: Option<String>,
    amount_minor: i64,
    currency: String,
    status: String,
    refunded_minor: i64,
    next_action: Option<SqlJson<Value>>,
    failure_reason: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<PaymentRow> for Payment {
    fn from(r: PaymentRow) -> Self {
        Self {
            id: r.id,
            reference: r.reference,
            method_id: r.method_id,
            provider: r.provider,
            provider_ref: r.provider_ref,
            amount_minor: r.amount_minor,
            currency: r.currency,
            status: r.status,
            refunded_minor: r.refunded_minor,
            next_action: r.next_action.map(|j| j.0),
            failure_reason: r.failure_reason,
            created_at: r.created_at,
            updated_at: r.updated_at,
            client_secret: None,
        }
    }
}

const COLUMNS: &str = "id, reference, method_id, provider, provider_ref, amount_minor, currency, \
     status, refunded_minor, next_action, failure_reason, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

/// Money returned from a payment.
#[model]
#[derive(sqlx::FromRow)]
pub struct Refund {
    /// e.g. `rfd_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub payment_id: String,
    pub amount_minor: i64,
    pub reason: String,
    pub provider_ref: Option<String>,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// What to collect.
#[derive(Debug, Clone)]
pub struct StartPayment {
    pub method_id: String,
    pub reference: String,
    pub amount_minor: i64,
    pub currency: Currency,
    pub return_url: Option<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_payments,
        create_payment,
        get_payment,
        mark_paid,
        capture_payment,
        cancel_payment,
        list_refunds,
        create_refund,
    ))
}

async fn lock(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Payment> {
    let id = PaymentId::parse(id).map_err(|_| Error::not_found())?;
    let row = sqlx::query_as::<_, PaymentRow>(&format!(
        "SELECT {COLUMNS} FROM payments WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    row.map(Into::into).ok_or_else(Error::not_found)
}

fn status_of(p: &Payment) -> Result<PaymentStatus> {
    p.status.parse().map_err(Error::internal)
}

async fn settings_of(conn: &mut PgConnection, scope: &impl Scope, p: &Payment) -> Result<Value> {
    let Some(method) = &p.method_id else {
        return Ok(serde_json::json!({}));
    };
    let row: Option<(SqlJson<Value>,)> =
        sqlx::query_as("SELECT settings FROM payment_methods WHERE tenant_id = $1 AND id = $2")
            .bind(scope.tenant_id())
            .bind(method)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map_or_else(|| serde_json::json!({}), |(s,)| s.0))
}

fn provider_of<'a>(providers: &'a Providers, p: &Payment) -> Result<&'a dyn PaymentProvider> {
    providers
        .get(&p.provider)
        .map(|p| p.as_ref())
        .ok_or_else(|| {
            Error::conflict(format!("the {} provider is not available", p.provider))
                .with_code("provider_unavailable")
        })
}

/// Writes `update` to a locked payment and publishes `payment.updated`
/// when its status changed. A move its status does not allow (a late or
/// repeated provider message) changes nothing.
async fn write(
    tx: &mut TenantTx,
    scope: &impl Scope,
    current: Payment,
    update: &ProviderUpdate,
) -> Result<Payment> {
    let from = status_of(&current)?;
    if !from.can_become(update.status) {
        return Ok(current);
    }
    let row = sqlx::query_as::<_, PaymentRow>(&format!(
        "UPDATE payments SET status = $3, provider_ref = COALESCE($4, provider_ref), \
         next_action = CASE WHEN $3 IN ('pending', 'requires_action') \
                       THEN COALESCE($5, next_action) ELSE NULL END, \
         failure_reason = $6, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(scope.tenant_id())
    .bind(&current.id)
    .bind(update.status.as_str())
    .bind(&update.provider_ref)
    .bind(update.next_action.as_ref().map(SqlJson))
    .bind(&update.failure_reason)
    .fetch_one(&mut **tx)
    .await?;
    let payment: Payment = row.into();
    if from != update.status {
        scope.publish(tx, "payment.updated", &payment).await?;
    }
    Ok(payment)
}

/// Starts collecting a payment with a method of the shop: records it as
/// `pending` (publishing `payment.created`), asks the provider, and
/// records its answer. A provider error leaves the payment `failed` with
/// the reason. An unknown or inactive method is a 422.
pub async fn start(
    db: &Db,
    providers: &Providers,
    scope: &impl Scope,
    req: &StartPayment,
) -> Result<Payment> {
    if req.amount_minor <= 0 {
        return Err(Error::unprocessable("the amount must be positive").with_code("invalid_amount"));
    }
    let unknown = || {
        Error::unprocessable(format!("no active payment method {}", req.method_id))
            .with_code("unknown_payment_method")
    };
    let method = PaymentMethodId::parse(&req.method_id).map_err(|_| unknown())?;
    let mut tx = scope.begin(db).await?;
    let row: Option<(String, SqlJson<Value>)> = sqlx::query_as(
        "SELECT provider, settings FROM payment_methods \
         WHERE tenant_id = $1 AND id = $2 AND active",
    )
    .bind(scope.tenant_id())
    .bind(method.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let (provider_name, settings) = row.ok_or_else(unknown)?;
    let provider = providers.get(&provider_name).ok_or_else(|| {
        Error::unprocessable(format!("the {provider_name} provider is not available"))
            .with_code("unknown_provider")
    })?;
    let id = PaymentId::new();
    let payment: Payment = sqlx::query_as::<_, PaymentRow>(&format!(
        "INSERT INTO payments (id, tenant_id, reference, method_id, provider, amount_minor, \
         currency, status) VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending') RETURNING {COLUMNS}"
    ))
    .bind(id.as_str())
    .bind(scope.tenant_id())
    .bind(&req.reference)
    .bind(method.as_str())
    .bind(&provider_name)
    .bind(req.amount_minor)
    .bind(req.currency.as_str())
    .fetch_one(&mut *tx)
    .await?
    .into();
    scope.publish(&mut tx, "payment.created", &payment).await?;
    tx.commit().await?;

    let ask = StartRequest {
        tenant_id: scope.tenant_id(),
        payment_id: id.as_str(),
        reference: &req.reference,
        amount_minor: req.amount_minor,
        currency: req.currency.as_str(),
        return_url: req.return_url.as_deref(),
        settings: &settings.0,
    };
    let update = match provider.start(&ask).await {
        Ok(u) => u,
        Err(e) => ProviderUpdate {
            failure_reason: Some(e.message().to_owned()),
            ..ProviderUpdate::status(PaymentStatus::Failed)
        },
    };
    let mut payment = apply(db, scope, id.as_str(), &update).await?;
    if matches!(payment.status.as_str(), "pending" | "requires_action") {
        payment.client_secret = update.buyer_secret;
    }
    Ok(payment)
}

/// One payment of the shop. Run it in the shop's transaction.
pub async fn get(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Option<Payment>> {
    let Ok(id) = PaymentId::parse(id) else {
        return Ok(None);
    };
    let row = sqlx::query_as::<_, PaymentRow>(&format!(
        "SELECT {COLUMNS} FROM payments WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(Into::into))
}

/// Records what a provider says about a payment (from its webhook or a
/// poll). Moves its status does not allow are ignored, so repeated and
/// late messages are harmless.
pub async fn apply(
    db: &Db,
    scope: &impl Scope,
    payment_id: &str,
    update: &ProviderUpdate,
) -> Result<Payment> {
    let mut tx = scope.begin(db).await?;
    let current = lock(&mut tx, scope, payment_id).await?;
    let payment = write(&mut tx, scope, current, update).await?;
    tx.commit().await?;
    Ok(payment)
}

/// The payments for a reference, newest first. Run it in the shop's
/// transaction.
pub async fn for_reference(
    conn: &mut PgConnection,
    scope: &impl Scope,
    reference: &str,
) -> Result<Vec<Payment>> {
    let rows = sqlx::query_as::<_, PaymentRow>(&format!(
        "SELECT {COLUMNS} FROM payments WHERE tenant_id = $1 AND reference = $2 ORDER BY id DESC"
    ))
    .bind(scope.tenant_id())
    .bind(reference)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// The shop's payments, newest first (`payments:read`). Filter with
/// `?reference=`, `?status=` or `?provider=`.
#[get("/api/payments", tag = "payments")]
async fn list_payments(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Payment>>> {
    let mut tx = tenant.begin_for(&db, &PAYMENTS_READ).await?;
    let page: Paged<PaymentRow> = TenantListing::new("payments", COLUMNS)
        .sortable(["id", "amount_minor"])
        .default_sort("-id")
        .filter_text("reference")
        .filter_text("status")
        .filter_text("provider")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    // Zapline follow-up: a `Paged::map`.
    Ok(Json(Paged {
        items: page.items.into_iter().map(Into::into).collect(),
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// Body of a payment staff start, e.g. for a phone order.
#[model]
pub struct NewPayment {
    pub method_id: String,
    #[validate(length(min = 1, max = 100))]
    pub reference: String,
    pub amount_minor: i64,
    pub currency: String,
    #[validate(url)]
    pub return_url: Option<String>,
}

/// Start a payment with one of the shop's methods (`payments:write`).
/// Checkout does this for buyers. Publishes `payment.created`, then
/// `payment.updated` as it moves.
#[post("/api/payments", tag = "payments", status = 201)]
async fn create_payment(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Valid(Json(p)): Valid<Json<NewPayment>>,
) -> Result<(StatusCode, Json<Payment>)> {
    let currency = Currency::parse(&p.currency)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))?;
    tenant
        .begin_for(&db, &PAYMENTS_WRITE)
        .await?
        .commit()
        .await?;
    let req = StartPayment {
        method_id: p.method_id,
        reference: p.reference,
        amount_minor: p.amount_minor,
        currency,
        return_url: p.return_url,
    };
    let payment = start(&db, &providers, &tenant, &req).await?;
    Ok((StatusCode::CREATED, Json(payment)))
}

/// One payment (`payments:read`).
#[get("/api/payments/{id}", tag = "payments")]
async fn get_payment(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Payment>> {
    let id = PaymentId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PAYMENTS_READ).await?;
    let row = sqlx::query_as::<_, PaymentRow>(&format!(
        "SELECT {COLUMNS} FROM payments WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    row.map(|r| Json(r.into())).ok_or_else(Error::not_found)
}

fn not_now(p: &Payment, action: &str) -> Error {
    Error::conflict(format!("a {} payment cannot be {action}", p.status))
        .with_code("invalid_payment_status")
}

/// Mark a payment paid when its money arrived, for providers staff
/// confirm such as manual (`payments:write`). Publishes `payment.updated`.
#[post("/api/payments/{id}/mark-paid", tag = "payments")]
async fn mark_paid(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
) -> Result<Json<Payment>> {
    let mut tx = tenant.begin_for(&db, &PAYMENTS_WRITE).await?;
    let p = lock(&mut tx, &tenant, &id).await?;
    if !provider_of(&providers, &p)?.confirmed_by_staff() {
        return Err(Error::conflict(format!(
            "{} payments are confirmed by the provider",
            p.provider
        ))
        .with_code("confirmed_by_provider"));
    }
    if !matches!(
        status_of(&p)?,
        PaymentStatus::Pending | PaymentStatus::RequiresAction | PaymentStatus::Authorized
    ) {
        return Err(not_now(&p, "marked paid"));
    }
    let p = write(
        &mut tx,
        &tenant,
        p,
        &ProviderUpdate::status(PaymentStatus::Paid),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(p))
}

fn provider_view<'a>(
    p: &'a Payment,
    tenant_id: &'a str,
    settings: &'a Value,
) -> ProviderPayment<'a> {
    ProviderPayment {
        tenant_id,
        payment_id: &p.id,
        provider_ref: p.provider_ref.as_deref(),
        amount_minor: p.amount_minor,
        currency: &p.currency,
        settings,
    }
}

/// Collect an authorized payment (`payments:write`). Publishes
/// `payment.updated`.
#[post("/api/payments/{id}/capture", tag = "payments")]
async fn capture_payment(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
) -> Result<Json<Payment>> {
    let mut tx = tenant.begin_for(&db, &PAYMENTS_WRITE).await?;
    let p = lock(&mut tx, &tenant, &id).await?;
    if status_of(&p)? != PaymentStatus::Authorized {
        return Err(not_now(&p, "captured"));
    }
    let provider = provider_of(&providers, &p)?;
    let settings = settings_of(&mut tx, &tenant, &p).await?;
    let update = provider
        .capture(&provider_view(&p, tenant.id(), &settings))
        .await?;
    let p = write(&mut tx, &tenant, p, &update).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Cancel a payment that was not collected (`payments:write`). Publishes
/// `payment.updated`.
#[post("/api/payments/{id}/cancel", tag = "payments")]
async fn cancel_payment(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
) -> Result<Json<Payment>> {
    let mut tx = tenant.begin_for(&db, &PAYMENTS_WRITE).await?;
    let p = lock(&mut tx, &tenant, &id).await?;
    let status = status_of(&p)?;
    if status == PaymentStatus::Canceled {
        return Ok(Json(p));
    }
    if !status.can_become(PaymentStatus::Canceled) {
        return Err(not_now(&p, "canceled"));
    }
    let provider = provider_of(&providers, &p)?;
    let settings = settings_of(&mut tx, &tenant, &p).await?;
    provider
        .cancel(&provider_view(&p, tenant.id(), &settings))
        .await?;
    let p = write(
        &mut tx,
        &tenant,
        p,
        &ProviderUpdate::status(PaymentStatus::Canceled),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(p))
}

const REFUND_COLUMNS: &str = "id, payment_id, amount_minor, reason, provider_ref, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

/// Refunds of a payment.
#[model]
pub struct RefundList {
    pub items: Vec<Refund>,
}

/// A payment's refunds, oldest first (`payments:read`).
#[get("/api/payments/{id}/refunds", tag = "payments")]
async fn list_refunds(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<RefundList>> {
    let mut tx = tenant.begin_for(&db, &PAYMENTS_READ).await?;
    let p = lock(&mut tx, &tenant, &id).await?;
    let items = sqlx::query_as::<_, Refund>(&format!(
        "SELECT {REFUND_COLUMNS} FROM payment_refunds WHERE tenant_id = $1 AND payment_id = $2 \
         ORDER BY id"
    ))
    .bind(tenant.id())
    .bind(&p.id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(RefundList { items }))
}

/// Body of a refund.
#[model]
pub struct NewRefund {
    /// Defaults to all that is left.
    pub amount_minor: Option<i64>,
    #[validate(length(max = 500))]
    pub reason: Option<String>,
}

/// The refund and the payment after it.
#[model]
pub struct RefundResult {
    pub refund: Refund,
    pub payment: Payment,
}

/// Return money from a collected payment (`payments:refund`). More than
/// is left is a 422. Publishes `payment.refunded` and `payment.updated`.
#[post("/api/payments/{id}/refunds", tag = "payments", status = 201)]
async fn create_refund(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
    Valid(Json(r)): Valid<Json<NewRefund>>,
) -> Result<(StatusCode, Json<RefundResult>)> {
    let mut tx = tenant.begin_for(&db, &PAYMENTS_REFUND).await?;
    let p = lock(&mut tx, &tenant, &id).await?;
    let status = status_of(&p)?;
    if !matches!(
        status,
        PaymentStatus::Paid | PaymentStatus::PartiallyRefunded
    ) {
        return Err(not_now(&p, "refunded"));
    }
    let left = p.amount_minor - p.refunded_minor;
    let amount = r.amount_minor.unwrap_or(left);
    if amount <= 0 || amount > left {
        return Err(
            Error::unprocessable(format!("refund 1 to {left}")).with_code("invalid_refund_amount")
        );
    }
    let provider = provider_of(&providers, &p)?;
    let settings = settings_of(&mut tx, &tenant, &p).await?;
    let provider_ref = provider
        .refund(&provider_view(&p, tenant.id(), &settings), amount)
        .await?;
    let refund = sqlx::query_as::<_, Refund>(&format!(
        "INSERT INTO payment_refunds (id, tenant_id, payment_id, amount_minor, reason, \
         provider_ref) VALUES ($1, $2, $3, $4, $5, $6) RETURNING {REFUND_COLUMNS}"
    ))
    .bind(RefundId::new().as_str())
    .bind(tenant.id())
    .bind(&p.id)
    .bind(amount)
    .bind(r.reason.as_deref().unwrap_or("").trim())
    .bind(&provider_ref)
    .fetch_one(&mut *tx)
    .await?;
    let refunded = p.refunded_minor + amount;
    let next = PaymentStatus::after_refunds(p.amount_minor, refunded);
    let payment: Payment = sqlx::query_as::<_, PaymentRow>(&format!(
        "UPDATE payments SET refunded_minor = $3, status = $4, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&p.id)
    .bind(refunded)
    .bind(next.as_str())
    .fetch_one(&mut *tx)
    .await?
    .into();
    tenant.publish(&mut tx, "payment.refunded", &refund).await?;
    tenant.publish(&mut tx, "payment.updated", &payment).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(RefundResult { refund, payment })))
}
