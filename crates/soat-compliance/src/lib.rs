//! Compliance: what a shop owes the people it sells to.
//!
//! **GDPR export and erasure.** Every module declares the tables in which
//! it keeps data about customers ([`soat_module::PersonalData`]). With
//! them, staff can:
//!
//! - hand a customer all their data as JSON (`GET
//!   /api/customers/{id}/personal-data`, art. 15 and 20);
//! - erase a customer (`POST /api/customers/{id}/erase`, art. 17). Each
//!   table's rows are deleted, anonymized, or kept for a legal reason
//!   (orders and invoices, for tax law), and the event log is redacted.
//!   The report says what happened to each table.
//!
//! A signed-in customer can download their own data and ask to be erased
//! (`/api/storefront/account/personal-data`,
//! `/api/storefront/account/erasure-request`). Staff then erase them, or
//! decline with a reason (`/api/privacy-requests`).
//!
//! **Cookie consent.** The shop's cookie banner and the tracking it holds
//! back until consent: see [`consent`].
//!
//! **Data retention.** How long each kind of data is kept, swept daily:
//! see [`retention`].

pub mod consent;
pub mod retention;

use serde_json::{json, Value};
use soat_core::{id_kind, Permission};
use soat_customers::accounts::SignedIn;
use soat_module::{Erase, Module, PersonalData, PersonalDataMap};
use soat_tenancy::{Scope, Shop, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

id_kind!(PrivacyRequestId, PrivacyRequestKind, "prq");

/// See customers' requests about their data.
pub const PRIVACY_READ: Permission =
    Permission::new("privacy:read", "See customers' requests about their data").for_staff();
/// Export a customer's personal data.
pub const PRIVACY_EXPORT: Permission =
    Permission::new("privacy:export", "Export a customer's personal data");
/// Erase a customer, or decline their request.
pub const PRIVACY_ERASE: Permission =
    Permission::new("privacy:erase", "Erase customers' personal data");

/// The compliance module.
pub struct Compliance;

impl Module for Compliance {
    fn name(&self) -> &'static str {
        "compliance"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "customers", "orders"]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[
            PRIVACY_READ,
            PRIVACY_EXPORT,
            PRIVACY_ERASE,
            consent::PRIVACY_SETTINGS,
        ]
    }

    fn retention(&self) -> &'static [soat_module::Retention] {
        &[
            soat_module::Retention {
                key: "consent_records",
                label: "Cookie choices (recorded without who made them)",
                table: "consent_records",
                expired: "created_at < now() - make_interval(days => $2)",
                default_days: Some(1095),
                min_days: 365,
            },
            soat_module::Retention {
                key: "closed_privacy_requests",
                label: "Customers' privacy requests, after they were closed",
                table: "privacy_requests",
                expired: "status <> 'open' AND closed_at < now() - make_interval(days => $2)",
                default_days: None,
                min_days: 365,
            },
        ]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            export_customer,
            erase_customer,
            my_personal_data,
            request_erasure,
            list_requests,
            decline_request,
        ))
        .mount((
            consent::get_settings,
            consent::put_settings,
            consent::stats,
            consent::storefront_settings,
            consent::storefront_record,
        ))
        .plugin(retention::mount)
    }
}

/// The condition of a declaration, with the tenant and both parameters
/// typed so a condition may use either.
fn condition(d: &PersonalData) -> String {
    format!(
        "tenant_id = $1 AND $2::text IS NOT NULL AND $3::text IS NOT NULL AND ({})",
        d.matches
    )
}

/// Everything the shop keeps about `customer` (with `email`), table by
/// table in module order.
///
/// # Errors
///
/// Database errors.
pub async fn export(
    conn: &mut PgConnection,
    scope: &impl Scope,
    map: &PersonalDataMap,
    customer: &str,
    email: &str,
) -> Result<Value> {
    let mut tables = serde_json::Map::new();
    for (_, d) in map.all() {
        if d.export.is_empty() {
            continue;
        }
        let (rows,): (Value,) = sqlx::query_as(&format!(
            "SELECT COALESCE(json_agg(t), '[]'::json) FROM (SELECT {} FROM {} WHERE {} LIMIT 10000) t",
            d.export.join(", "),
            d.table,
            condition(d)
        ))
        .bind(scope.tenant_id())
        .bind(customer)
        .bind(email.to_lowercase())
        .fetch_one(&mut *conn)
        .await?;
        tables.insert(d.table.to_owned(), rows);
    }
    let (now,): (String,) = sqlx::query_as(
        "SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(json!({ "customer_id": customer, "exported_at": now, "tables": tables }))
}

/// What erasing did to one table.
#[model]
pub struct Erased {
    pub module: String,
    pub table: String,
    /// `deleted`, `anonymized` or `kept`.
    pub action: String,
    pub rows: i64,
    /// Why rows were kept.
    pub reason: Option<String>,
}

/// What erasing a customer did.
#[model]
pub struct ErasureReport {
    pub customer_id: String,
    pub tables: Vec<Erased>,
    /// Events whose personal data was blanked.
    pub events_redacted: i64,
}

/// Erases `customer` (with `email`) from every declared table, dependents
/// first (modules in reverse order), and redacts the event log.
///
/// # Errors
///
/// Database errors.
pub async fn erase(
    tx: &mut TenantTx,
    scope: &impl Scope,
    map: &PersonalDataMap,
    customer: &str,
    email: &str,
) -> Result<ErasureReport> {
    let email = email.to_lowercase();
    let mut tables = Vec::new();
    for (module, d) in map.all().iter().rev() {
        let (action, rows, reason) = match d.erase {
            Erase::Delete => {
                let done = sqlx::query(&format!("DELETE FROM {} WHERE {}", d.table, condition(d)))
                    .bind(scope.tenant_id())
                    .bind(customer)
                    .bind(&email)
                    .execute(&mut **tx)
                    .await?;
                ("deleted", done.rows_affected(), None)
            }
            Erase::Anonymize(set) => {
                let set: Vec<String> = set.iter().map(|(c, v)| format!("{c} = {v}")).collect();
                let done = sqlx::query(&format!(
                    "UPDATE {} SET {} WHERE {}",
                    d.table,
                    set.join(", "),
                    condition(d)
                ))
                .bind(scope.tenant_id())
                .bind(customer)
                .bind(&email)
                .execute(&mut **tx)
                .await?;
                ("anonymized", done.rows_affected(), None)
            }
            Erase::Keep(why) => {
                let (n,): (i64,) = sqlx::query_as(&format!(
                    "SELECT count(*) FROM {} WHERE {}",
                    d.table,
                    condition(d)
                ))
                .bind(scope.tenant_id())
                .bind(customer)
                .bind(&email)
                .fetch_one(&mut **tx)
                .await?;
                ("kept", u64::try_from(n).unwrap_or(0), Some(why.to_owned()))
            }
        };
        tables.push(Erased {
            module: (*module).to_owned(),
            table: d.table.to_owned(),
            action: action.to_owned(),
            rows: i64::try_from(rows).unwrap_or(i64::MAX),
            reason,
        });
    }
    tables.reverse();
    let events_redacted = redact_events(tx, scope, customer, &email).await?;
    Ok(ErasureReport {
        customer_id: customer.to_owned(),
        tables,
        events_redacted,
    })
}

/// Blanks the personal data of events that mention `customer` or `email`.
async fn redact_events(
    tx: &mut TenantTx,
    scope: &impl Scope,
    customer: &str,
    email: &str,
) -> Result<i64> {
    let found: Vec<(String, Value)> = sqlx::query_as(
        "SELECT id, payload FROM events WHERE tenant_id = $1 \
         AND (strpos(payload::text, $2) > 0 OR ($3 <> '' AND strpos(lower(payload::text), $3) > 0))",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .bind(email)
    .fetch_all(&mut **tx)
    .await?;
    let mut n = 0;
    for (id, mut payload) in found {
        if soat_core::privacy::redact(&mut payload, email) {
            sqlx::query("UPDATE events SET payload = $3 WHERE tenant_id = $1 AND id = $2")
                .bind(scope.tenant_id())
                .bind(&id)
                .bind(&payload)
                .execute(&mut **tx)
                .await?;
            n += 1;
        }
    }
    Ok(n)
}

/// Everything the shop keeps about a customer, as JSON for them
/// (`privacy:export`). Publishes `customer.data_exported`.
#[get("/api/customers/{id}/personal-data", tag = "privacy")]
async fn export_customer(
    db: Db,
    tenant: Tenant,
    map: State<PersonalDataMap>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let mut tx = tenant.begin_for(&db, &PRIVACY_EXPORT).await?;
    let c = soat_customers::load(&mut tx, &tenant, &id).await?;
    let out = export(&mut tx, &tenant, &map, &c.id, &c.email).await?;
    tenant
        .publish(&mut tx, "customer.data_exported", &json!({ "id": c.id }))
        .await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// The customer's email, typed again: erasing cannot be undone.
#[model]
pub struct EraseInput {
    pub email: String,
}

/// Erase a customer (`privacy:erase`), after typing their email again:
/// their account, addresses, carts, reviews, wishlist, points and the
/// like are deleted; orders and discount uses stay without them;
/// invoices stay as they are, for tax law; the event log is redacted.
/// Their open requests are closed as done. Publishes `customer.erased`.
#[post("/api/customers/{id}/erase", tag = "privacy")]
async fn erase_customer(
    db: Db,
    tenant: Tenant,
    map: State<PersonalDataMap>,
    Path(id): Path<String>,
    Json(input): Json<EraseInput>,
) -> Result<Json<ErasureReport>> {
    let mut tx = tenant.begin_for(&db, &PRIVACY_ERASE).await?;
    let c = soat_customers::load(&mut tx, &tenant, &id).await?;
    if !input.email.trim().eq_ignore_ascii_case(c.email.trim()) {
        return Err(
            Error::unprocessable("type the customer's email to erase them")
                .with_code("email_mismatch"),
        );
    }
    let report = erase(&mut tx, &tenant, &map, &c.id, &c.email).await?;
    sqlx::query(
        "UPDATE privacy_requests SET status = 'done', closed_at = now() \
         WHERE tenant_id = $1 AND customer_id = $2 AND status = 'open'",
    )
    .bind(tenant.id())
    .bind(&c.id)
    .execute(&mut *tx)
    .await?;
    let rows: i64 = report
        .tables
        .iter()
        .filter(|t| t.action != "kept")
        .map(|t| t.rows)
        .sum();
    tenant
        .publish(
            &mut tx,
            "customer.erased",
            &json!({ "id": c.id, "rows": rows, "events_redacted": report.events_redacted }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(report))
}

/// Everything the shop keeps about the signed-in customer, as JSON.
#[get("/api/storefront/account/personal-data", tag = "storefront")]
async fn my_personal_data(
    db: Db,
    signed_in: SignedIn,
    map: State<PersonalDataMap>,
) -> Result<Json<Value>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let out = personal_data_of(&mut tx, &signed_in.shop, &map, &customer).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// The export of `customer`, signed in to `shop`, for the storefront.
/// Publishes `customer.data_exported`.
///
/// # Errors
///
/// Database errors.
pub async fn personal_data_of(
    tx: &mut TenantTx,
    shop: &Shop,
    map: &PersonalDataMap,
    customer: &str,
) -> Result<Value> {
    let (email,): (String,) =
        sqlx::query_as("SELECT email FROM customers WHERE tenant_id = $1 AND id = $2")
            .bind(shop.id())
            .bind(customer)
            .fetch_one(&mut **tx)
            .await?;
    let out = export(tx, shop, map, customer, &email).await?;
    shop.publish(
        tx,
        Some(customer),
        "customer.data_exported",
        &json!({ "id": customer }),
    )
    .await?;
    Ok(out)
}

/// A customer's request about their data.
#[model]
#[derive(sqlx::FromRow)]
pub struct PrivacyRequest {
    /// e.g. `prq_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub customer_id: String,
    /// The customer's email while they exist.
    pub email: Option<String>,
    /// `erase`.
    pub kind: String,
    /// `open`, `done` or `declined`.
    pub status: String,
    /// Why it was declined.
    pub note: String,
    pub created_at: String,
    pub closed_at: Option<String>,
}

const REQUEST: &str = "r.id, r.customer_id, c.email, r.kind, r.status, r.note, \
     to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(r.closed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS closed_at \
     FROM privacy_requests r LEFT JOIN customers c ON c.tenant_id = r.tenant_id \
     AND c.id = r.customer_id";

/// Ask the shop to erase the signed-in customer's data. Asking again while
/// one is open answers 200 with it. Publishes `privacy_request.created`.
#[post(
    "/api/storefront/account/erasure-request",
    tag = "storefront",
    status = 201
)]
async fn request_erasure(
    db: Db,
    signed_in: SignedIn,
) -> Result<(StatusCode, Json<PrivacyRequest>)> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let (status, r) = ask_erasure(&mut tx, &signed_in.shop, &customer).await?;
    tx.commit().await?;
    Ok((status, Json(r)))
}

/// Records `customer`'s request to be erased, for the storefront: 201 with
/// a new one, 200 with the one already open. Publishes
/// `privacy_request.created`.
///
/// # Errors
///
/// Database errors.
pub async fn ask_erasure(
    tx: &mut TenantTx,
    shop: &Shop,
    customer: &str,
) -> Result<(StatusCode, PrivacyRequest)> {
    let open: Option<PrivacyRequest> = sqlx::query_as(&format!(
        "SELECT {REQUEST} WHERE r.tenant_id = $1 AND r.customer_id = $2 AND r.status = 'open'"
    ))
    .bind(shop.id())
    .bind(customer)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(r) = open {
        return Ok((StatusCode::OK, r));
    }
    let id = PrivacyRequestId::new();
    sqlx::query(
        "INSERT INTO privacy_requests (id, tenant_id, customer_id, kind) VALUES ($1, $2, $3, 'erase')",
    )
    .bind(id.as_str())
    .bind(shop.id())
    .bind(customer)
    .execute(&mut **tx)
    .await?;
    let r: PrivacyRequest = sqlx::query_as(&format!(
        "SELECT {REQUEST} WHERE r.tenant_id = $1 AND r.id = $2"
    ))
    .bind(shop.id())
    .bind(id.as_str())
    .fetch_one(&mut **tx)
    .await?;
    shop.publish(
        tx,
        Some(customer),
        "privacy_request.created",
        &json!({ "id": r.id, "customer_id": customer, "kind": r.kind }),
    )
    .await?;
    Ok((StatusCode::CREATED, r))
}

#[model]
pub struct PrivacyRequestList {
    pub items: Vec<PrivacyRequest>,
}

/// Which requests to list.
#[model]
pub struct RequestsQuery {
    /// `open`, `done` or `declined`.
    pub status: Option<String>,
}

/// Customers' requests about their data, oldest open first
/// (`privacy:read`).
#[get("/api/privacy-requests", tag = "privacy")]
async fn list_requests(
    db: Db,
    tenant: Tenant,
    Query(q): Query<RequestsQuery>,
) -> Result<Json<PrivacyRequestList>> {
    let mut tx = tenant.begin_for(&db, &PRIVACY_READ).await?;
    let items = sqlx::query_as(&format!(
        "SELECT {REQUEST} WHERE r.tenant_id = $1 AND ($2::text IS NULL OR r.status = $2) \
         ORDER BY r.status <> 'open', r.created_at, r.id LIMIT 200"
    ))
    .bind(tenant.id())
    .bind(&q.status)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(PrivacyRequestList { items }))
}

/// Why a request is declined.
#[model]
pub struct Decline {
    #[validate(length(min = 1, max = 1000))]
    pub note: String,
}

/// Decline an open request, with the reason (`privacy:erase`), e.g. an
/// unpaid order still owed. Publishes `privacy_request.declined`.
#[post("/api/privacy-requests/{id}/decline", tag = "privacy")]
async fn decline_request(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(d)): Valid<Json<Decline>>,
) -> Result<Json<PrivacyRequest>> {
    let id = PrivacyRequestId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PRIVACY_ERASE).await?;
    let done = sqlx::query(
        "UPDATE privacy_requests SET status = 'declined', note = $3, closed_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND status = 'open'",
    )
    .bind(tenant.id())
    .bind(id.as_str())
    .bind(d.note.trim())
    .execute(&mut *tx)
    .await?;
    let r: Option<PrivacyRequest> = sqlx::query_as(&format!(
        "SELECT {REQUEST} WHERE r.tenant_id = $1 AND r.id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let r = r.ok_or_else(Error::not_found)?;
    if done.rows_affected() == 0 {
        return Err(Error::conflict("the request is closed").with_code("request_closed"));
    }
    tenant
        .publish(
            &mut tx,
            "privacy_request.declined",
            &json!({ "id": r.id, "customer_id": r.customer_id }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(r))
}
