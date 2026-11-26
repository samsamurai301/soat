//! Carriers: accounts that print shipping labels.
//!
//! An adapter implements [`CarrierProvider`] (Sendcloud, behind the
//! `sendcloud` feature, reaches DHL, DPD, GLS, PostNL, UPS and more from
//! one account) and is registered on the module ([`crate::Shipping::with`]).
//! A shop adds an account with its own credentials, sealed with the
//! install's secret key and never shown again; orders print labels for
//! shipments with [`create_label`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use soat_core::id_kind;
use soat_core::secret::Sealer;
use soat_tenancy::{Scope, Tenant, TenantTx};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::Bytes;

use crate::{SHIPPING_MANAGE, SHIPPING_READ};

id_kind!(CarrierAccountId, CarrierAccountKind, "cra");

/// What an adapter call returns.
pub type CarrierFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A shipment to print a label for.
#[derive(Debug, Clone)]
pub struct LabelRequest<'a> {
    /// The account's credentials, unsealed.
    pub credentials: &'a Value,
    /// The account's settings (e.g. which service to use).
    pub settings: &'a Value,
    /// The postal address it goes to, as an order keeps it.
    pub to: &'a Value,
    pub email: &'a str,
    pub weight_grams: i64,
    /// What the shop calls it, e.g. the order number.
    pub reference: &'a str,
}

/// A printed label.
#[derive(Debug, Clone)]
pub struct CarrierLabel {
    /// The carrier that takes it, e.g. `dhl`.
    pub carrier: String,
    pub tracking_number: String,
    pub tracking_url: String,
    /// The adapter's id for it, to cancel it.
    pub carrier_ref: String,
    /// The label, a PDF.
    pub pdf: Bytes,
}

/// A label printer: a carrier, or a platform for many.
pub trait CarrierProvider: Send + Sync {
    /// e.g. `sendcloud`.
    fn name(&self) -> &'static str;
    /// e.g. `Sendcloud`.
    fn label(&self) -> &'static str;
    /// Checks an account's credentials and settings before they are kept.
    fn check(&self, credentials: &Value, settings: &Value) -> Result<()>;
    /// Prints a label.
    fn create_label<'a>(&'a self, req: &'a LabelRequest<'a>) -> CarrierFuture<'a, CarrierLabel>;
    /// Voids a label not yet used.
    fn cancel_label<'a>(
        &'a self,
        credentials: &'a Value,
        carrier_ref: &'a str,
    ) -> CarrierFuture<'a, ()>;
}

/// The adapters of this install and the key that seals credentials; an
/// app state.
#[derive(Clone)]
pub struct Carriers {
    list: Arc<Vec<Arc<dyn CarrierProvider>>>,
    sealer: Option<Sealer>,
}

impl Carriers {
    pub(crate) fn new(list: Vec<Arc<dyn CarrierProvider>>, sealer: Option<Sealer>) -> Self {
        Self {
            list: Arc::new(list),
            sealer,
        }
    }

    /// The adapter called `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn CarrierProvider>> {
        self.list.iter().find(|p| p.name() == name)
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

/// A shop's carrier account.
#[model]
pub struct CarrierAccount {
    /// e.g. `cra_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// The adapter, e.g. `sendcloud`.
    pub provider: String,
    /// Unique in the shop, e.g. `Sendcloud DHL`.
    pub name: String,
    /// The adapter's settings, e.g. `{"shipping_method_id": 8}`.
    pub settings: Value,
    pub active: bool,
}

/// Body of a create request.
#[model]
pub struct NewCarrierAccount {
    pub provider: String,
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    #[serde(default)]
    pub settings: Value,
    /// Kept sealed, never shown again; e.g. API keys.
    pub credentials: Value,
}

/// Body of an update request.
#[model]
pub struct CarrierAccountChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    pub settings: Option<Value>,
    /// Replaces the credentials.
    pub credentials: Option<Value>,
    pub active: Option<bool>,
}

/// An adapter this install has.
#[model]
pub struct CarrierInfo {
    pub name: String,
    pub label: String,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_carriers,
        list_accounts,
        create_account,
        get_account,
        update_account,
        delete_account,
    ))
}

type Row = (String, String, String, SqlJson<Value>, bool, Vec<u8>);

const COLUMNS: &str = "id, provider, name, settings, active, credentials";

fn account(r: Row) -> CarrierAccount {
    CarrierAccount {
        id: r.0,
        provider: r.1,
        name: r.2,
        settings: r.3 .0,
        active: r.4,
    }
}

async fn row(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Row> {
    let id = CarrierAccountId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLUMNS} FROM carrier_accounts WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)
}

fn settings(v: Value) -> Result<Value> {
    match v {
        Value::Null => Ok(Value::Object(Default::default())),
        Value::Object(_) => Ok(v),
        _ => Err(Error::unprocessable("settings are an object").with_code("invalid_settings")),
    }
}

fn unknown(provider: &str) -> Error {
    Error::unprocessable(format!("this install has no carrier {provider}"))
        .with_code("unknown_carrier")
}

/// The carrier adapters this install has (`shipping:read`).
#[get("/api/carriers", tag = "shipping")]
async fn list_carriers(
    db: Db,
    tenant: Tenant,
    carriers: State<Carriers>,
) -> Result<Json<Vec<CarrierInfo>>> {
    tenant
        .begin_for(&db, &SHIPPING_READ)
        .await?
        .rollback()
        .await?;
    Ok(Json(
        carriers
            .list
            .iter()
            .map(|p| CarrierInfo {
                name: p.name().to_owned(),
                label: p.label().to_owned(),
            })
            .collect(),
    ))
}

/// The shop's carrier accounts (`shipping:read`); never their credentials.
#[get("/api/carrier-accounts", tag = "shipping")]
async fn list_accounts(db: Db, tenant: Tenant) -> Result<Json<Vec<CarrierAccount>>> {
    let mut tx = tenant.begin_for(&db, &SHIPPING_READ).await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM carrier_accounts WHERE tenant_id = $1 ORDER BY name"
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(rows.into_iter().map(account).collect()))
}

/// Add a carrier account (`shipping:manage`): the adapter checks the
/// credentials and settings (422), which are sealed and never shown again
/// (503 `secrets_unavailable` when the install keeps no secrets).
/// Publishes `carrier_account.created`, without the credentials.
#[post("/api/carrier-accounts", tag = "shipping", status = 201)]
async fn create_account(
    db: Db,
    tenant: Tenant,
    carriers: State<Carriers>,
    Valid(Json(b)): Valid<Json<NewCarrierAccount>>,
) -> Result<(StatusCode, Json<CarrierAccount>)> {
    let provider = carriers
        .get(&b.provider)
        .ok_or_else(|| unknown(&b.provider))?;
    let settings = settings(b.settings)?;
    provider.check(&b.credentials, &settings)?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let sealed = carriers
        .sealer()?
        .seal(b.credentials.to_string().as_bytes(), tenant.id());
    let r: Row = sqlx::query_as(&format!(
        "INSERT INTO carrier_accounts (id, tenant_id, provider, name, settings, credentials) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
    ))
    .bind(CarrierAccountId::new().as_str())
    .bind(tenant.id())
    .bind(provider.name())
    .bind(b.name.trim())
    .bind(SqlJson(&settings))
    .bind(&sealed)
    .fetch_one(&mut *tx)
    .await?;
    let a = account(r);
    tenant
        .publish(&mut tx, "carrier_account.created", &a)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(a)))
}

/// One carrier account (`shipping:read`).
#[get("/api/carrier-accounts/{id}", tag = "shipping")]
async fn get_account(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<CarrierAccount>> {
    let mut tx = tenant.begin_for(&db, &SHIPPING_READ).await?;
    let r = row(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(account(r)))
}

/// Rename an account, change its settings or credentials, or switch it
/// off (`shipping:manage`). Publishes `carrier_account.updated`.
#[patch("/api/carrier-accounts/{id}", tag = "shipping")]
async fn update_account(
    db: Db,
    tenant: Tenant,
    carriers: State<Carriers>,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CarrierAccountChange>>,
) -> Result<Json<CarrierAccount>> {
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let current = row(&mut tx, &tenant, &id).await?;
    let provider = carriers
        .get(&current.1)
        .ok_or_else(|| unknown(&current.1))?;
    let settings = match c.settings {
        Some(s) => settings(s)?,
        None => current.3 .0.clone(),
    };
    let sealed = match &c.credentials {
        Some(creds) => {
            provider.check(creds, &settings)?;
            Some(
                carriers
                    .sealer()?
                    .seal(creds.to_string().as_bytes(), tenant.id()),
            )
        }
        None => {
            let creds = open(&carriers, &tenant, &current.5)?;
            provider.check(&creds, &settings)?;
            None
        }
    };
    let r: Row = sqlx::query_as(&format!(
        "UPDATE carrier_accounts SET name = COALESCE($3, name), settings = $4, \
         credentials = COALESCE($5, credentials), active = COALESCE($6, active) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.0)
    .bind(c.name.as_deref().map(str::trim))
    .bind(SqlJson(&settings))
    .bind(sealed)
    .bind(c.active)
    .fetch_one(&mut *tx)
    .await?;
    let a = account(r);
    tenant
        .publish(&mut tx, "carrier_account.updated", &a)
        .await?;
    tx.commit().await?;
    Ok(Json(a))
}

/// Remove a carrier account (`shipping:manage`); labels printed with it
/// stay. Publishes `carrier_account.deleted`.
#[delete("/api/carrier-accounts/{id}", tag = "shipping", status = 204)]
async fn delete_account(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let current = row(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM carrier_accounts WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&current.0)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(&mut tx, "carrier_account.deleted", &account(current))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn open(carriers: &Carriers, scope: &impl Scope, sealed: &[u8]) -> Result<Value> {
    let raw = carriers
        .sealer()?
        .open(sealed, scope.tenant_id())
        .ok_or_else(|| {
            Error::conflict("the account's credentials cannot be read: enter them again")
                .with_code("credentials_unreadable")
        })?;
    serde_json::from_slice(&raw).map_err(|e| Error::internal(e.to_string()))
}

/// The shipment a label is for.
pub struct LabelParts<'a> {
    pub to: &'a Value,
    pub email: &'a str,
    pub weight_grams: i64,
    pub reference: &'a str,
}

/// Prints a label with the shop's active account `account_id`: 422
/// `unknown_carrier_account` for none such. Run it in the tenant
/// transaction. Returns the account and the label.
pub async fn create_label(
    tx: &mut TenantTx,
    scope: &impl Scope,
    carriers: &Carriers,
    account_id: &str,
    parts: LabelParts<'_>,
) -> Result<(CarrierAccount, CarrierLabel)> {
    let r = row(tx, scope, account_id).await.map_err(|_| {
        Error::unprocessable(format!("no carrier account {account_id}"))
            .with_code("unknown_carrier_account")
    })?;
    if !r.4 {
        return Err(Error::unprocessable("that carrier account is switched off")
            .with_code("unknown_carrier_account"));
    }
    let provider = carriers.get(&r.1).ok_or_else(|| unknown(&r.1))?;
    let credentials = open(carriers, scope, &r.5)?;
    let req = LabelRequest {
        credentials: &credentials,
        settings: &r.3 .0,
        to: parts.to,
        email: parts.email,
        weight_grams: parts.weight_grams,
        reference: parts.reference,
    };
    let label = provider.create_label(&req).await?;
    Ok((account(r), label))
}

/// Voids label `carrier_ref` printed with account `account_id`. Run it in
/// the tenant transaction.
pub async fn cancel_label(
    tx: &mut TenantTx,
    scope: &impl Scope,
    carriers: &Carriers,
    account_id: &str,
    carrier_ref: &str,
) -> Result<()> {
    let r = row(tx, scope, account_id).await?;
    let provider = carriers.get(&r.1).ok_or_else(|| unknown(&r.1))?;
    let credentials = open(carriers, scope, &r.5)?;
    provider.cancel_label(&credentials, carrier_ref).await
}
