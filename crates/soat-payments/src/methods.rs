//! Payment methods: the ways a shop takes money.

use serde_json::Value;
use soat_core::id_kind;
use soat_tenancy::{Shop, Tenant};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{Providers, PAYMENTS_MANAGE, PAYMENTS_READ};

id_kind!(PaymentMethodId, PaymentMethodKind, "pm");

/// A way the shop takes money.
#[model]
pub struct PaymentMethod {
    /// e.g. `pm_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// e.g. `manual` or `stripe`; see `GET /api/payment-providers`.
    pub provider: String,
    /// Unique in the shop; what buyers see, e.g. `Bank transfer`.
    pub name: String,
    /// The provider's settings; never secrets.
    pub settings: Value,
    pub active: bool,
    /// Lower comes first at checkout.
    pub position: i32,
}

#[derive(sqlx::FromRow)]
pub(crate) struct MethodRow {
    id: String,
    provider: String,
    name: String,
    settings: SqlJson<Value>,
    active: bool,
    position: i32,
}

impl From<MethodRow> for PaymentMethod {
    fn from(r: MethodRow) -> Self {
        Self {
            id: r.id,
            provider: r.provider,
            name: r.name,
            settings: r.settings.0,
            active: r.active,
            position: r.position,
        }
    }
}

pub(crate) const METHOD_COLUMNS: &str = "id, provider, name, settings, active, position";

/// Body of a method creation.
#[model]
pub struct NewMethod {
    pub provider: String,
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub settings: Option<Value>,
    pub active: Option<bool>,
    pub position: Option<i32>,
}

/// Body of a method change.
#[model]
pub struct MethodChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    /// Replaces the settings.
    pub settings: Option<Value>,
    pub active: Option<bool>,
    pub position: Option<i32>,
}

/// A list of methods.
#[model]
pub struct MethodList {
    pub items: Vec<PaymentMethod>,
}

/// A method as buyers see it.
#[model]
pub struct PublicMethod {
    pub id: String,
    pub provider: String,
    pub name: String,
}

/// The methods buyers can choose.
#[model]
pub struct PublicMethodList {
    pub items: Vec<PublicMethod>,
}

/// A provider this server can use.
#[model]
pub struct ProviderInfo {
    pub name: String,
    pub label: String,
    /// Whether staff mark its payments paid by hand.
    pub confirmed_by_staff: bool,
}

/// The providers.
#[model]
pub struct ProviderList {
    pub items: Vec<ProviderInfo>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_providers,
        list_methods,
        create_method,
        update_method,
        delete_method,
        storefront_methods,
    ))
}

fn checked_settings(providers: &Providers, provider: &str, settings: &Value) -> Result<()> {
    let p = providers.get(provider).ok_or_else(|| {
        Error::unprocessable(format!("no payment provider {provider}"))
            .with_code("unknown_provider")
    })?;
    if serde_json::to_vec(settings).map_or(0, |v| v.len()) > 16 * 1024 {
        return Err(
            Error::unprocessable("settings are at most 16 KiB").with_code("invalid_settings")
        );
    }
    p.check_settings(settings)
}

/// The payment providers this server can use (`payments:read`).
#[get("/api/payment-providers", tag = "payments")]
async fn list_providers(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
) -> Result<Json<ProviderList>> {
    tenant
        .begin_for(&db, &PAYMENTS_READ)
        .await?
        .commit()
        .await?;
    let items = providers
        .iter()
        .map(|p| ProviderInfo {
            name: p.name().to_owned(),
            label: p.label().to_owned(),
            confirmed_by_staff: p.confirmed_by_staff(),
        })
        .collect();
    Ok(Json(ProviderList { items }))
}

/// The shop's payment methods, in checkout order (`payments:read`).
#[get("/api/payment-methods", tag = "payments")]
async fn list_methods(db: Db, tenant: Tenant) -> Result<Json<MethodList>> {
    let mut tx = tenant.begin_for(&db, &PAYMENTS_READ).await?;
    let rows = sqlx::query_as::<_, MethodRow>(&format!(
        "SELECT {METHOD_COLUMNS} FROM payment_methods WHERE tenant_id = $1 \
         ORDER BY position, name"
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(MethodList {
        items: rows.into_iter().map(Into::into).collect(),
    }))
}

/// Add a payment method (`payments:manage`). An unknown provider or
/// settings it rejects are a 422. Publishes `payment_method.created`.
#[post("/api/payment-methods", tag = "payments", status = 201)]
async fn create_method(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Valid(Json(m)): Valid<Json<NewMethod>>,
) -> Result<(StatusCode, Json<PaymentMethod>)> {
    let settings = m.settings.unwrap_or_else(|| serde_json::json!({}));
    checked_settings(&providers, &m.provider, &settings)?;
    let mut tx = tenant.begin_for(&db, &PAYMENTS_MANAGE).await?;
    let method: PaymentMethod = sqlx::query_as::<_, MethodRow>(&format!(
        "INSERT INTO payment_methods (id, tenant_id, provider, name, settings, active, position) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {METHOD_COLUMNS}"
    ))
    .bind(PaymentMethodId::new().as_str())
    .bind(tenant.id())
    .bind(&m.provider)
    .bind(m.name.trim())
    .bind(SqlJson(&settings))
    .bind(m.active.unwrap_or(true))
    .bind(m.position.unwrap_or(0))
    .fetch_one(&mut *tx)
    .await?
    .into();
    tenant
        .publish(&mut tx, "payment_method.created", &method)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(method)))
}

/// Change a payment method (`payments:manage`). Publishes
/// `payment_method.updated`.
#[patch("/api/payment-methods/{id}", tag = "payments")]
async fn update_method(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
    Valid(Json(m)): Valid<Json<MethodChange>>,
) -> Result<Json<PaymentMethod>> {
    let id = PaymentMethodId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PAYMENTS_MANAGE).await?;
    let provider: Option<(String,)> =
        sqlx::query_as("SELECT provider FROM payment_methods WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    let (provider,) = provider.ok_or_else(Error::not_found)?;
    if let Some(s) = &m.settings {
        checked_settings(&providers, &provider, s)?;
    }
    let method: PaymentMethod = sqlx::query_as::<_, MethodRow>(&format!(
        "UPDATE payment_methods SET name = COALESCE($3, name), \
         settings = COALESCE($4, settings), active = COALESCE($5, active), \
         position = COALESCE($6, position) WHERE tenant_id = $1 AND id = $2 \
         RETURNING {METHOD_COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .bind(m.name.as_deref().map(str::trim))
    .bind(m.settings.as_ref().map(SqlJson))
    .bind(m.active)
    .bind(m.position)
    .fetch_one(&mut *tx)
    .await?
    .into();
    tenant
        .publish(&mut tx, "payment_method.updated", &method)
        .await?;
    tx.commit().await?;
    Ok(Json(method))
}

/// Delete a payment method; its payments keep their provider
/// (`payments:manage`). Publishes `payment_method.deleted`.
#[delete("/api/payment-methods/{id}", tag = "payments", status = 204)]
async fn delete_method(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = PaymentMethodId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PAYMENTS_MANAGE).await?;
    let gone = sqlx::query("DELETE FROM payment_methods WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "payment_method.deleted",
            &serde_json::json!({ "id": id.as_str() }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The payment methods a buyer can choose, in order.
#[get("/api/storefront/payment-methods", tag = "storefront")]
async fn storefront_methods(db: Db, shop: Shop) -> Result<Json<PublicMethodList>> {
    let mut tx = shop.begin(&db).await?;
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, provider, name FROM payment_methods WHERE tenant_id = $1 AND active \
         ORDER BY position, name",
    )
    .bind(shop.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(PublicMethodList {
        items: rows
            .into_iter()
            .map(|(id, provider, name)| PublicMethod { id, provider, name })
            .collect(),
    }))
}
