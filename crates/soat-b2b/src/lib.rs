//! B2B: businesses that buy from the shop.
//!
//! A **company** (`/api/companies`) groups the customers who buy for a
//! business. Its members get the company's price tag, so the shop's price
//! lists for that customer group apply to them in carts, checkouts and
//! draft orders. A company can have payment terms (orders due N days after
//! they are placed) and a credit limit; every member's order is entered in
//! the company's ledger, which gives what it owes, what is overdue and how
//! much credit is left. A company on hold cannot accept quotes.
//!
//! A **quote** is a buyer asking for a price: they send lines and a note
//! (`POST /api/storefront/account/quotes`); staff answer with an offer,
//! which is a draft order with the prices they choose, and the buyer
//! accepts it (the order is placed, due after the company's terms) or
//! declines it.

use std::sync::Arc;

use serde_json::json;
use soat_checkout::drafts::{self, CompleteDraft, DraftInput, DraftLineInput, DraftOrder};
use soat_checkout::TaxProviders;
use soat_core::{id_kind, Currency, Permission};
use soat_customers::accounts::SignedIn;
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_payments::{Payment, Providers};
use soat_tenancy::{EventShop, Scope, Shop, Tenant, TenantTx};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::Resources;

id_kind!(CompanyId, CompanyKind, "com");
id_kind!(QuoteId, QuoteKind, "quo");

/// See companies, their ledgers and quotes.
pub const B2B_READ: Permission =
    Permission::new("companies:read", "See companies, what they owe, and quotes").for_staff();
/// Change companies and their members, and answer quotes.
pub const B2B_MANAGE: Permission = Permission::new(
    "companies:manage",
    "Change companies and their members, and answer quotes",
)
.for_staff();

/// Most lines in a quote.
const MAX_LINES: usize = 100;

/// The B2B module.
pub struct B2b;

impl Module for B2b {
    fn name(&self) -> &'static str {
        "b2b"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "catalog",
            "customers",
            "payments",
            "orders",
            "checkout",
        ]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[B2B_READ, B2B_MANAGE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(Ledger)]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_companies,
            create_company,
            get_company,
            update_company,
            list_members,
            put_member,
            delete_member,
            company_balance,
        ))
        .mount((
            list_quotes,
            get_quote,
            offer_quote,
            decline_quote,
            my_company,
            request_quote,
            my_quotes,
            my_quote,
            accept_my_quote,
            decline_my_quote,
        ))
    }
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_b2b")
}

fn timestamp(column: &str, name: &str) -> String {
    format!("to_char({column} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS {name}")
}

// ---------------------------------------------------------------- companies

/// A business that buys from the shop.
#[model]
#[derive(sqlx::FromRow)]
pub struct Company {
    /// e.g. `com_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub name: String,
    pub vat_number: Option<String>,
    /// The customer tag its members get, for group prices.
    pub price_tag: Option<String>,
    /// Days an order may stay unpaid; 0 is pay now.
    pub payment_terms_days: i32,
    pub currency: String,
    /// Most it may owe at once; none is no limit.
    pub credit_limit_minor: Option<i64>,
    /// `active` or `on_hold`.
    pub status: String,
    pub note: String,
    pub members: i64,
    pub created_at: String,
    pub updated_at: String,
}

fn company_columns() -> String {
    format!(
        "c.id, c.name, c.vat_number, c.price_tag, c.payment_terms_days, c.currency, \
         c.credit_limit_minor, c.status, c.note, \
         (SELECT count(*) FROM company_members m WHERE m.tenant_id = c.tenant_id \
          AND m.company_id = c.id) AS members, {}, {}",
        timestamp("c.created_at", "created_at"),
        timestamp("c.updated_at", "updated_at")
    )
}

#[model]
pub struct CompanyList {
    pub items: Vec<Company>,
}

/// A new company.
#[model]
pub struct NewCompany {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    #[validate(length(max = 20))]
    pub vat_number: Option<String>,
    #[validate(length(max = 100))]
    pub price_tag: Option<String>,
    #[validate(range(min = 0, max = 180))]
    pub payment_terms_days: Option<i32>,
    /// Defaults to the shop's main currency.
    pub currency: Option<String>,
    #[validate(range(min = 0))]
    pub credit_limit_minor: Option<i64>,
    #[validate(length(max = 2000))]
    pub note: Option<String>,
}

/// Changes to a company; what is left out stays. An empty `price_tag`,
/// `vat_number` or a negative `credit_limit_minor` clears it.
#[model]
pub struct CompanyChange {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    #[validate(length(max = 20))]
    pub vat_number: Option<String>,
    #[validate(length(max = 100))]
    pub price_tag: Option<String>,
    #[validate(range(min = 0, max = 180))]
    pub payment_terms_days: Option<i32>,
    pub credit_limit_minor: Option<i64>,
    /// `active` or `on_hold`.
    pub status: Option<String>,
    #[validate(length(max = 2000))]
    pub note: Option<String>,
}

/// A tag as customers keep it: lower case, trimmed; none when empty.
fn price_tag(raw: Option<&str>) -> Option<String> {
    raw.map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
}

fn vat(raw: Option<&str>) -> Option<String> {
    raw.map(|v| v.trim().to_uppercase().replace(' ', ""))
        .filter(|v| !v.is_empty())
}

async fn company(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Company> {
    let id = CompanyId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {} FROM companies c WHERE c.tenant_id = $1 AND c.id = $2",
        company_columns()
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// The company customer `customer` buys for, and their role in it.
///
/// # Errors
///
/// Database errors.
pub async fn company_of(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
) -> Result<Option<(Company, String)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT company_id, role FROM company_members WHERE tenant_id = $1 AND customer_id = $2",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        Some((id, role)) => Ok(Some((company(conn, scope, &id).await?, role))),
        None => Ok(None),
    }
}

/// The shop's companies, by name (`companies:read`); `?q=` finds them by name or
/// VAT number.
#[get("/api/companies", tag = "b2b")]
async fn list_companies(
    db: Db,
    tenant: Tenant,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<CompanyList>> {
    let mut tx = tenant.begin_for(&db, &B2B_READ).await?;
    let search = q.get("q").map(|s| format!("%{}%", s.trim()));
    let items = sqlx::query_as(&format!(
        "SELECT {} FROM companies c WHERE c.tenant_id = $1 \
         AND ($2::text IS NULL OR c.name ILIKE $2 OR c.vat_number ILIKE $2) \
         ORDER BY c.name, c.id LIMIT 500",
        company_columns()
    ))
    .bind(tenant.id())
    .bind(search)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(CompanyList { items }))
}

/// Add a company (`companies:manage`). Publishes `company.created`.
#[post("/api/companies", tag = "b2b", status = 201)]
async fn create_company(
    db: Db,
    tenant: Tenant,
    Valid(Json(n)): Valid<Json<NewCompany>>,
) -> Result<(StatusCode, Json<Company>)> {
    let mut tx = tenant.begin_for(&db, &B2B_MANAGE).await?;
    let currency = match n.currency.as_deref() {
        Some(c) => Some(c.to_owned()),
        None => soat_catalog::storefront::main_currency(&mut tx, &tenant).await?,
    }
    .unwrap_or_else(|| "EUR".to_owned());
    let currency = Currency::parse(&currency)
        .map_err(|e| invalid(e.to_string()))?
        .as_str()
        .to_owned();
    let id = CompanyId::new();
    sqlx::query(
        "INSERT INTO companies (id, tenant_id, name, vat_number, price_tag, \
         payment_terms_days, currency, credit_limit_minor, note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(n.name.trim())
    .bind(vat(n.vat_number.as_deref()))
    .bind(price_tag(n.price_tag.as_deref()))
    .bind(n.payment_terms_days.unwrap_or(0))
    .bind(&currency)
    .bind(n.credit_limit_minor)
    .bind(n.note.as_deref().unwrap_or("").trim())
    .execute(&mut *tx)
    .await?;
    let c = company(&mut tx, &tenant, id.as_str()).await?;
    tenant.publish(&mut tx, "company.created", &c).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(c)))
}

/// One company (`companies:read`).
#[get("/api/companies/{id}", tag = "b2b")]
async fn get_company(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Company>> {
    let mut tx = tenant.begin_for(&db, &B2B_READ).await?;
    let c = company(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(c))
}

/// The ids of company `id`'s members.
async fn member_ids(tx: &mut TenantTx, scope: &impl Scope, id: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT customer_id FROM company_members WHERE tenant_id = $1 AND company_id = $2",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|(c,)| c).collect())
}

/// Change a company (`companies:manage`). A new price tag moves its members to
/// it. Publishes `company.updated`.
#[patch("/api/companies/{id}", tag = "b2b")]
async fn update_company(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CompanyChange>>,
) -> Result<Json<Company>> {
    if let Some(s) = c.status.as_deref() {
        if !["active", "on_hold"].contains(&s) {
            return Err(invalid("status is active or on_hold"));
        }
    }
    let mut tx = tenant.begin_for(&db, &B2B_MANAGE).await?;
    let before = company(&mut tx, &tenant, &id).await?;
    let tag = match c.price_tag.as_deref() {
        Some(t) => price_tag(Some(t)),
        None => before.price_tag.clone(),
    };
    let vat_number = match c.vat_number.as_deref() {
        Some(v) => vat(Some(v)),
        None => before.vat_number.clone(),
    };
    let limit = match c.credit_limit_minor {
        Some(l) if l < 0 => None,
        Some(l) => Some(l),
        None => before.credit_limit_minor,
    };
    sqlx::query(
        "UPDATE companies SET name = COALESCE($3, name), vat_number = $4, price_tag = $5, \
         payment_terms_days = COALESCE($6, payment_terms_days), credit_limit_minor = $7, \
         status = COALESCE($8, status), note = COALESCE($9, note), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&before.id)
    .bind(c.name.as_deref().map(str::trim))
    .bind(&vat_number)
    .bind(&tag)
    .bind(c.payment_terms_days)
    .bind(limit)
    .bind(&c.status)
    .bind(c.note.as_deref().map(str::trim))
    .execute(&mut *tx)
    .await?;
    if tag != before.price_tag {
        for member in member_ids(&mut tx, &tenant, &before.id).await? {
            if let Some(old) = &before.price_tag {
                soat_customers::set_tag(&mut tx, &tenant, &member, old, false).await?;
            }
            if let Some(new) = &tag {
                soat_customers::set_tag(&mut tx, &tenant, &member, new, true).await?;
            }
        }
    }
    let after = company(&mut tx, &tenant, &before.id).await?;
    tenant.publish(&mut tx, "company.updated", &after).await?;
    tx.commit().await?;
    Ok(Json(after))
}

/// A customer who buys for a company.
#[model]
#[derive(sqlx::FromRow)]
pub struct Member {
    pub customer_id: String,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    /// `admin` (sees the company's orders and what it owes) or `buyer`.
    pub role: String,
    pub created_at: String,
}

#[model]
pub struct MemberList {
    pub items: Vec<Member>,
}

/// A member's role.
#[model]
pub struct MemberRole {
    /// `admin` or `buyer`; defaults to `buyer`.
    pub role: Option<String>,
}

async fn members(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Vec<Member>> {
    Ok(sqlx::query_as(&format!(
        "SELECT m.customer_id, cu.email, COALESCE(cu.first_name, '') AS first_name, \
         COALESCE(cu.last_name, '') AS last_name, m.role, {} \
         FROM company_members m JOIN customers cu ON cu.id = m.customer_id \
         WHERE m.tenant_id = $1 AND m.company_id = $2 ORDER BY cu.email LIMIT 1000",
        timestamp("m.created_at", "created_at")
    ))
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_all(conn)
    .await?)
}

/// A company's members (`companies:read`).
#[get("/api/companies/{id}/members", tag = "b2b")]
async fn list_members(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<MemberList>> {
    let mut tx = tenant.begin_for(&db, &B2B_READ).await?;
    let c = company(&mut tx, &tenant, &id).await?;
    let items = members(&mut tx, &tenant, &c.id).await?;
    tx.commit().await?;
    Ok(Json(MemberList { items }))
}

/// Make customer `customer_id` a member of the company, or change their
/// role (`companies:manage`). A customer is in one company at most: one in
/// another is a 409 `in_another_company`. They get the company's price
/// tag. Publishes `company.member_added` or `company.member_updated`.
#[put("/api/companies/{id}/members/{customer_id}", tag = "b2b")]
async fn put_member(
    db: Db,
    tenant: Tenant,
    Path((id, customer)): Path<(String, String)>,
    Json(r): Json<MemberRole>,
) -> Result<Json<MemberList>> {
    let role = r.role.unwrap_or_else(|| "buyer".to_owned());
    if !["admin", "buyer"].contains(&role.as_str()) {
        return Err(invalid("role is admin or buyer"));
    }
    let mut tx = tenant.begin_for(&db, &B2B_MANAGE).await?;
    let c = company(&mut tx, &tenant, &id).await?;
    let found = soat_customers::load(&mut tx, &tenant, &customer).await?;
    let current: Option<(String,)> = sqlx::query_as(
        "SELECT company_id FROM company_members WHERE tenant_id = $1 AND customer_id = $2",
    )
    .bind(tenant.id())
    .bind(&found.id)
    .fetch_optional(&mut *tx)
    .await?;
    let kind = match current {
        Some((other,)) if other != c.id => {
            return Err(Error::conflict("the customer buys for another company")
                .with_code("in_another_company"))
        }
        Some(_) => {
            sqlx::query(
                "UPDATE company_members SET role = $3 WHERE tenant_id = $1 AND customer_id = $2",
            )
            .bind(tenant.id())
            .bind(&found.id)
            .bind(&role)
            .execute(&mut *tx)
            .await?;
            "company.member_updated"
        }
        None => {
            sqlx::query(
                "INSERT INTO company_members (tenant_id, company_id, customer_id, role) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(tenant.id())
            .bind(&c.id)
            .bind(&found.id)
            .bind(&role)
            .execute(&mut *tx)
            .await?;
            if let Some(tag) = &c.price_tag {
                soat_customers::set_tag(&mut tx, &tenant, &found.id, tag, true).await?;
            }
            "company.member_added"
        }
    };
    tenant
        .publish(
            &mut tx,
            kind,
            &json!({ "company_id": c.id, "customer_id": found.id, "role": role }),
        )
        .await?;
    let items = members(&mut tx, &tenant, &c.id).await?;
    tx.commit().await?;
    Ok(Json(MemberList { items }))
}

/// Take a customer out of the company (`companies:manage`); they lose its price
/// tag. Publishes `company.member_removed`.
#[delete("/api/companies/{id}/members/{customer_id}", tag = "b2b", status = 204)]
async fn delete_member(
    db: Db,
    tenant: Tenant,
    Path((id, customer)): Path<(String, String)>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &B2B_MANAGE).await?;
    let c = company(&mut tx, &tenant, &id).await?;
    let gone = sqlx::query(
        "DELETE FROM company_members WHERE tenant_id = $1 AND company_id = $2 AND customer_id = $3",
    )
    .bind(tenant.id())
    .bind(&c.id)
    .bind(&customer)
    .execute(&mut *tx)
    .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    if let Some(tag) = &c.price_tag {
        soat_customers::set_tag(&mut tx, &tenant, &customer, tag, false).await?;
    }
    tenant
        .publish(
            &mut tx,
            "company.member_removed",
            &json!({ "company_id": c.id, "customer_id": customer }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------- ledger

/// One of a company's orders in its ledger.
#[model]
#[derive(sqlx::FromRow)]
pub struct CompanyOrder {
    pub order_id: String,
    pub number: i64,
    pub total_minor: i64,
    pub currency: String,
    pub placed_at: String,
    pub due_at: String,
    pub paid_at: Option<String>,
    /// `open`, `overdue`, `paid` or `closed` (canceled or refunded).
    pub state: String,
}

/// What a company owes.
#[model]
pub struct Balance {
    pub company_id: String,
    pub currency: String,
    /// Unpaid orders, due or not.
    pub open_minor: i64,
    /// Unpaid orders past their due date.
    pub overdue_minor: i64,
    pub credit_limit_minor: Option<i64>,
    /// Credit left; none without a limit.
    pub available_minor: Option<i64>,
    /// The latest orders first.
    pub orders: Vec<CompanyOrder>,
}

/// Company `c`'s balance, with its latest 100 orders.
///
/// # Errors
///
/// Database errors.
pub async fn balance(conn: &mut PgConnection, scope: &impl Scope, c: &Company) -> Result<Balance> {
    let (open, overdue): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(sum(total_minor), 0)::bigint, \
         COALESCE(sum(total_minor) FILTER (WHERE due_at < now()), 0)::bigint \
         FROM company_orders WHERE tenant_id = $1 AND company_id = $2 AND currency = $3 \
         AND paid_at IS NULL AND NOT closed",
    )
    .bind(scope.tenant_id())
    .bind(&c.id)
    .bind(&c.currency)
    .fetch_one(&mut *conn)
    .await?;
    let orders = sqlx::query_as(&format!(
        "SELECT order_id, number, total_minor, currency, {}, {}, {}, \
         CASE WHEN closed THEN 'closed' WHEN paid_at IS NOT NULL THEN 'paid' \
              WHEN due_at < now() THEN 'overdue' ELSE 'open' END AS state \
         FROM company_orders WHERE tenant_id = $1 AND company_id = $2 \
         ORDER BY placed_at DESC LIMIT 100",
        timestamp("placed_at", "placed_at"),
        timestamp("due_at", "due_at"),
        timestamp("paid_at", "paid_at"),
    ))
    .bind(scope.tenant_id())
    .bind(&c.id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Balance {
        company_id: c.id.clone(),
        currency: c.currency.clone(),
        open_minor: open,
        overdue_minor: overdue,
        credit_limit_minor: c.credit_limit_minor,
        available_minor: c.credit_limit_minor.map(|l| (l - open).max(0)),
        orders,
    })
}

/// What a company owes, what is overdue and its credit left (`companies:read`).
#[get("/api/companies/{id}/balance", tag = "b2b")]
async fn company_balance(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Balance>> {
    let mut tx = tenant.begin_for(&db, &B2B_READ).await?;
    let c = company(&mut tx, &tenant, &id).await?;
    let b = balance(&mut tx, &tenant, &c).await?;
    tx.commit().await?;
    Ok(Json(b))
}

/// Enters members' orders in their company's ledger and keeps them up to
/// date as they are paid, canceled or refunded. Idempotent.
struct Ledger;

impl Subscriber for Ledger {
    fn name(&self) -> &'static str {
        "b2b-ledger"
    }

    fn wants(&self, kind: &str) -> bool {
        matches!(
            kind,
            "order.placed" | "order.paid" | "order.updated" | "order.canceled"
        )
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let o = &event.payload;
            let Some(order_id) = o["id"].as_str() else {
                return Ok(());
            };
            let pool: PgPool = res.require()?;
            let scope = EventShop(event.tenant_id.clone());
            let mut tx = scope.begin_on(&pool).await?;
            if event.kind == "order.placed" {
                let Some(customer) = o["customer_id"].as_str() else {
                    return Ok(());
                };
                sqlx::query(
                    "INSERT INTO company_orders (tenant_id, order_id, company_id, number, \
                     total_minor, currency, placed_at, due_at) \
                     SELECT $1, $2, c.id, $4, $5, $6, $7::timestamptz, \
                            $7::timestamptz + make_interval(days => c.payment_terms_days) \
                     FROM company_members m JOIN companies c ON c.id = m.company_id \
                     WHERE m.tenant_id = $1 AND m.customer_id = $3 \
                     ON CONFLICT (tenant_id, order_id) DO NOTHING",
                )
                .bind(&event.tenant_id)
                .bind(order_id)
                .bind(customer)
                .bind(o["number"].as_i64().unwrap_or(0))
                .bind(o["total_minor"].as_i64().unwrap_or(0))
                .bind(o["currency"].as_str().unwrap_or(""))
                .bind(o["placed_at"].as_str().unwrap_or(""))
                .execute(&mut *tx)
                .await?;
            }
            let paid = matches!(
                o["financial_status"].as_str(),
                Some("paid" | "partially_refunded")
            );
            let closed = event.kind == "order.canceled"
                || o["status"] == "canceled"
                || o["financial_status"] == "refunded";
            sqlx::query(
                "UPDATE company_orders SET \
                 paid_at = CASE WHEN $3 THEN COALESCE(paid_at, now()) ELSE paid_at END, \
                 closed = closed OR $4 WHERE tenant_id = $1 AND order_id = $2",
            )
            .bind(&event.tenant_id)
            .bind(order_id)
            .bind(paid || event.kind == "order.paid")
            .bind(closed)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        })
    }
}

// ------------------------------------------------------------------- quotes

/// A line a buyer asks a price for.
#[model]
pub struct QuoteLine {
    /// A product without variants, or a variant.
    pub item_id: String,
    #[validate(range(min = 1, max = 100000))]
    pub quantity: i64,
    /// Filled in from the catalog.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub sku: String,
}

/// A buyer's request for a price, and the shop's answer.
#[model]
pub struct Quote {
    /// e.g. `quo_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub customer_id: String,
    pub company_id: Option<String>,
    pub lines: Vec<QuoteLine>,
    pub note: String,
    /// `requested`, `offered`, `accepted` or `declined`.
    pub status: String,
    /// The offer, once there is one.
    pub draft_order_id: Option<String>,
    /// The offer's totals, once there is one.
    pub offer: Option<DraftOrder>,
    /// The order, once accepted.
    pub order_id: Option<String>,
    pub expires_at: Option<String>,
    /// The shop's words with its offer or refusal.
    pub reply: String,
    pub created_at: String,
}

#[derive(sqlx::FromRow)]
struct QuoteRow {
    id: String,
    customer_id: String,
    company_id: Option<String>,
    lines: SqlJson<Vec<QuoteLine>>,
    note: String,
    status: String,
    draft_order_id: Option<String>,
    order_id: Option<String>,
    expires_at: Option<String>,
    expired: bool,
    reply: String,
    created_at: String,
}

fn quote_columns() -> String {
    format!(
        "id, customer_id, company_id, lines, note, status, draft_order_id, order_id, {}, \
         COALESCE(expires_at < now(), FALSE) AS expired, reply, {}",
        timestamp("expires_at", "expires_at"),
        timestamp("created_at", "created_at")
    )
}

#[model]
pub struct QuoteList {
    pub items: Vec<Quote>,
}

/// What a buyer asks a price for.
#[model]
pub struct QuoteRequest {
    #[validate(length(min = 1, max = 100))]
    pub lines: Vec<QuoteLine>,
    #[validate(length(max = 2000))]
    pub note: Option<String>,
}

/// The shop's offer: a price per line (else what the buyer would pay
/// today), shipping, a discount and how long it holds.
#[model]
pub struct Offer {
    /// The lines to offer; the request's when left out.
    pub lines: Option<Vec<DraftLineInput>>,
    #[validate(range(min = 0))]
    pub shipping_price_minor: Option<i64>,
    #[validate(range(min = 0))]
    pub discount_minor: Option<i64>,
    /// Days the offer holds; 30 when left out.
    #[validate(range(min = 1, max = 365))]
    pub expires_in_days: Option<i32>,
    #[validate(length(max = 2000))]
    pub reply: Option<String>,
}

/// The shop's words when it refuses a quote.
#[model]
pub struct Refusal {
    #[validate(length(max = 2000))]
    pub reply: Option<String>,
}

/// How the buyer pays for an accepted offer.
#[model]
pub struct Acceptance {
    /// One of the shop's payment methods; none leaves the order to be paid
    /// within the company's terms.
    pub payment_method_id: Option<String>,
    #[validate(url)]
    pub return_url: Option<String>,
}

/// The order placed from an accepted quote.
#[model]
pub struct Accepted {
    pub quote: Quote,
    pub order_id: String,
    /// Send as `x-soat-order-token` to see the order as the buyer.
    pub order_token: String,
    pub payment: Option<Payment>,
}

async fn quote_row(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
    lock: bool,
) -> Result<QuoteRow> {
    let id = QuoteId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {} FROM quotes WHERE tenant_id = $1 AND id = $2{}",
        quote_columns(),
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

async fn shown(
    conn: &mut PgConnection,
    scope: &impl Scope,
    taxes: &TaxProviders,
    r: QuoteRow,
) -> Result<Quote> {
    let offer = match (&r.draft_order_id, r.status.as_str()) {
        (Some(d), "offered" | "accepted") => Some(drafts::get(conn, scope, taxes, d).await?),
        _ => None,
    };
    Ok(Quote {
        id: r.id,
        customer_id: r.customer_id,
        company_id: r.company_id,
        lines: r.lines.0,
        note: r.note,
        status: r.status,
        draft_order_id: r.draft_order_id,
        offer,
        order_id: r.order_id,
        expires_at: r.expires_at,
        reply: r.reply,
        created_at: r.created_at,
    })
}

async fn load_quote(
    conn: &mut PgConnection,
    scope: &impl Scope,
    taxes: &TaxProviders,
    id: &str,
) -> Result<Quote> {
    let r = quote_row(conn, scope, id, false).await?;
    shown(conn, scope, taxes, r).await
}

async fn quotes_where(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<Quote>> {
    let rows: Vec<QuoteRow> = sqlx::query_as(&format!(
        "SELECT {} FROM quotes WHERE tenant_id = $1 \
         AND ($2::text IS NULL OR customer_id = $2) AND ($3::text IS NULL OR status = $3) \
         ORDER BY id DESC LIMIT 200",
        quote_columns()
    ))
    .bind(scope.tenant_id())
    .bind(customer)
    .bind(status)
    .fetch_all(&mut *conn)
    .await?;
    // Lists leave the offer's totals out; one quote has them.
    Ok(rows
        .into_iter()
        .map(|r| Quote {
            id: r.id,
            customer_id: r.customer_id,
            company_id: r.company_id,
            lines: r.lines.0,
            note: r.note,
            status: r.status,
            draft_order_id: r.draft_order_id,
            offer: None,
            order_id: r.order_id,
            expires_at: r.expires_at,
            reply: r.reply,
            created_at: r.created_at,
        })
        .collect())
}

/// The shop's quotes, newest first (`companies:read`); `?status=` filters.
#[get("/api/quotes", tag = "b2b")]
async fn list_quotes(
    db: Db,
    tenant: Tenant,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<QuoteList>> {
    let mut tx = tenant.begin_for(&db, &B2B_READ).await?;
    let items = quotes_where(
        &mut tx,
        &tenant,
        q.get("customer_id").map(String::as_str),
        q.get("status").map(String::as_str),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(QuoteList { items }))
}

/// One quote, with its offer's totals (`companies:read`).
#[get("/api/quotes/{id}", tag = "b2b")]
async fn get_quote(
    db: Db,
    tenant: Tenant,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
) -> Result<Json<Quote>> {
    let mut tx = tenant.begin_for(&db, &B2B_READ).await?;
    let q = load_quote(&mut tx, &tenant, &taxes, &id).await?;
    tx.commit().await?;
    Ok(Json(q))
}

fn not_open() -> Error {
    Error::conflict("the quote was answered already").with_code("quote_closed")
}

/// Answer a quote with an offer (`companies:manage`): a draft order for the
/// buyer at the prices given, to their saved address, with the company's
/// VAT number. Offering again replaces the offer. Publishes
/// `quote.offered`.
#[post("/api/quotes/{id}/offer", tag = "b2b")]
async fn offer_quote(
    db: Db,
    tenant: Tenant,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
    Valid(Json(o)): Valid<Json<Offer>>,
) -> Result<Json<Quote>> {
    let mut tx = tenant.begin_for(&db, &B2B_MANAGE).await?;
    let r = quote_row(&mut tx, &tenant, &id, true).await?;
    if !["requested", "offered"].contains(&r.status.as_str()) {
        return Err(not_open());
    }
    let lines = match o.lines {
        Some(l) => l,
        None => r
            .lines
            .0
            .iter()
            .map(|l| DraftLineInput {
                item_id: l.item_id.clone(),
                quantity: l.quantity,
                unit_price_minor: None,
            })
            .collect(),
    };
    let company = match &r.company_id {
        Some(c) => Some(company(&mut tx, &tenant, c).await?),
        None => None,
    };
    let currency = match &company {
        Some(c) => c.currency.clone(),
        None => soat_catalog::storefront::main_currency(&mut tx, &tenant)
            .await?
            .unwrap_or_else(|| "EUR".to_owned()),
    };
    let address = drafts::saved_address(&mut tx, &tenant, &r.customer_id).await?;
    let input = DraftInput {
        customer_id: Some(r.customer_id.clone()),
        email: None,
        currency,
        lines,
        shipping_address: address.clone(),
        billing_address: address,
        vat_number: company.as_ref().and_then(|c| c.vat_number.clone()),
        shipping_name: o.shipping_price_minor.map(|_| "Shipping".to_owned()),
        shipping_price_minor: o.shipping_price_minor,
        discount_minor: o.discount_minor,
        note: Some(format!("Quote {}", r.id)),
    };
    let draft = drafts::create(&mut tx, &tenant, &taxes, &input).await?;
    sqlx::query(
        "UPDATE quotes SET status = 'offered', draft_order_id = $3, reply = $4, \
         expires_at = now() + make_interval(days => $5), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&r.id)
    .bind(&draft.id)
    .bind(o.reply.as_deref().unwrap_or("").trim())
    .bind(o.expires_in_days.unwrap_or(30))
    .execute(&mut *tx)
    .await?;
    let q = load_quote(&mut tx, &tenant, &taxes, &r.id).await?;
    tenant.publish(&mut tx, "quote.offered", &q).await?;
    tx.commit().await?;
    Ok(Json(q))
}

async fn close(
    tx: &mut TenantTx,
    scope: &impl Scope,
    taxes: &TaxProviders,
    r: &QuoteRow,
    reply: Option<&str>,
) -> Result<Quote> {
    if !["requested", "offered"].contains(&r.status.as_str()) {
        return Err(not_open());
    }
    sqlx::query(
        "UPDATE quotes SET status = 'declined', reply = COALESCE($3, reply), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(scope.tenant_id())
    .bind(&r.id)
    .bind(reply.map(str::trim))
    .execute(&mut **tx)
    .await?;
    let q = load_quote(tx, scope, taxes, &r.id).await?;
    scope.publish(tx, "quote.declined", &q).await?;
    Ok(q)
}

/// Refuse a quote (`companies:manage`). Publishes `quote.declined`.
#[post("/api/quotes/{id}/decline", tag = "b2b")]
async fn decline_quote(
    db: Db,
    tenant: Tenant,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
    Valid(Json(f)): Valid<Json<Refusal>>,
) -> Result<Json<Quote>> {
    let mut tx = tenant.begin_for(&db, &B2B_MANAGE).await?;
    let r = quote_row(&mut tx, &tenant, &id, true).await?;
    let q = close(&mut tx, &tenant, &taxes, &r, f.reply.as_deref()).await?;
    tx.commit().await?;
    Ok(Json(q))
}

// --------------------------------------------------------------- storefront

/// The signed-in buyer's company.
#[model]
pub struct MyCompany {
    pub company: Company,
    /// `admin` or `buyer`.
    pub role: String,
    /// What the company owes; for its admins only.
    pub balance: Option<Balance>,
    /// Its people; for its admins only.
    pub members: Option<Vec<Member>>,
}

/// The company the signed-in customer buys for; 404 when none. Admins of
/// the company also see what it owes and its people.
#[get("/api/storefront/account/company", tag = "storefront")]
async fn my_company(db: Db, signed_in: SignedIn) -> Result<Json<MyCompany>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let (c, role) = company_of(&mut tx, &signed_in.shop, &customer)
        .await?
        .ok_or_else(Error::not_found)?;
    let (balance, people) = if role == "admin" {
        (
            Some(balance(&mut tx, &signed_in.shop, &c).await?),
            Some(members(&mut tx, &signed_in.shop, &c.id).await?),
        )
    } else {
        (None, None)
    };
    tx.commit().await?;
    Ok(Json(MyCompany {
        company: c,
        role,
        balance,
        members: people,
    }))
}

/// Asks the shop for a price on `lines` for customer `customer`. Publishes
/// `quote.requested`.
///
/// # Errors
///
/// 422 `invalid_b2b` for an item the shop does not sell or too many lines;
/// database errors.
pub async fn request(
    tx: &mut TenantTx,
    shop: &Shop,
    taxes: &TaxProviders,
    customer: &str,
    r: QuoteRequest,
) -> Result<Quote> {
    if r.lines.is_empty() || r.lines.len() > MAX_LINES {
        return Err(invalid(format!("a quote has 1 to {MAX_LINES} lines")));
    }
    let mut lines = Vec::with_capacity(r.lines.len());
    for l in r.lines {
        if !(1..=100_000).contains(&l.quantity) {
            return Err(invalid("a quantity is 1 to 100000"));
        }
        let item = soat_catalog::stock_item(tx, shop, &l.item_id)
            .await
            .map_err(|e| invalid(e.message().to_owned()))?
            .ok_or_else(|| invalid(format!("the shop sells no item {}", l.item_id)))?;
        lines.push(QuoteLine {
            item_id: item.id,
            quantity: l.quantity,
            title: item.title,
            sku: item.sku,
        });
    }
    let company = company_of(tx, shop, customer).await?.map(|(c, _)| c.id);
    let id = QuoteId::new();
    sqlx::query(
        "INSERT INTO quotes (id, tenant_id, customer_id, company_id, lines, note) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id.as_str())
    .bind(shop.id())
    .bind(customer)
    .bind(&company)
    .bind(SqlJson(&lines))
    .bind(r.note.as_deref().unwrap_or("").trim())
    .execute(&mut **tx)
    .await?;
    let q = load_quote(tx, shop, taxes, id.as_str()).await?;
    Scope::publish(shop, tx, "quote.requested", &q).await?;
    Ok(q)
}

/// Ask the shop for a price: lines (items and quantities) and a note.
/// Publishes `quote.requested`.
#[post("/api/storefront/account/quotes", tag = "storefront", status = 201)]
async fn request_quote(
    db: Db,
    signed_in: SignedIn,
    taxes: State<TaxProviders>,
    Valid(Json(r)): Valid<Json<QuoteRequest>>,
) -> Result<(StatusCode, Json<Quote>)> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let q = request(&mut tx, &signed_in.shop, &taxes, &customer, r).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(q)))
}

/// Customer `customer`'s quotes, newest first.
///
/// # Errors
///
/// Database errors.
pub async fn of_customer(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
) -> Result<Vec<Quote>> {
    quotes_where(conn, scope, Some(customer), None).await
}

/// The signed-in buyer's quotes, newest first.
#[get("/api/storefront/account/quotes", tag = "storefront")]
async fn my_quotes(db: Db, signed_in: SignedIn) -> Result<Json<QuoteList>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let items = of_customer(&mut tx, &signed_in.shop, &customer).await?;
    tx.commit().await?;
    Ok(Json(QuoteList { items }))
}

/// Customer `customer`'s quote `id`, locked; 404 for anyone else's.
async fn mine(tx: &mut TenantTx, shop: &Shop, customer: &str, id: &str) -> Result<QuoteRow> {
    let r = quote_row(tx, shop, id, true).await?;
    if r.customer_id != customer {
        return Err(Error::not_found());
    }
    Ok(r)
}

/// Customer `customer`'s quote `id`, with its offer's totals.
///
/// # Errors
///
/// 404 for an unknown quote or someone else's; database errors.
pub async fn my(
    conn: &mut PgConnection,
    shop: &Shop,
    taxes: &TaxProviders,
    customer: &str,
    id: &str,
) -> Result<Quote> {
    let q = load_quote(conn, shop, taxes, id).await?;
    if q.customer_id != customer {
        return Err(Error::not_found());
    }
    Ok(q)
}

/// One of the signed-in buyer's quotes, with the offer's totals.
#[get("/api/storefront/account/quotes/{id}", tag = "storefront")]
async fn my_quote(
    db: Db,
    signed_in: SignedIn,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
) -> Result<Json<Quote>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let q = my(&mut tx, &signed_in.shop, &taxes, &customer, &id).await?;
    tx.commit().await?;
    Ok(Json(q))
}

/// Accepts customer `customer`'s offered quote `id`: its order is placed,
/// and its payment started with `a.payment_method_id` (none leaves it to
/// be paid within the company's terms). Publishes `quote.accepted`.
///
/// # Errors
///
/// 404 for someone else's quote; 409 `quote_closed` when it is not on
/// offer, `quote_expired` after the offer ran out, `company_on_hold`, or
/// `credit_limit_exceeded` when the order would take the company over its
/// limit; the order's errors (e.g. 409 `insufficient_stock`); database
/// errors.
pub async fn accept(
    db: &Db,
    shop: &Shop,
    taxes: &TaxProviders,
    providers: &Providers,
    customer: &str,
    id: &str,
    a: Acceptance,
) -> Result<Accepted> {
    let mut tx = shop.begin(db).await?;
    let r = mine(&mut tx, shop, customer, id).await?;
    if r.status != "offered" {
        return Err(not_open());
    }
    if r.expired {
        return Err(
            Error::conflict("the offer ran out; ask for a new one").with_code("quote_expired")
        );
    }
    let draft_id = r.draft_order_id.clone().ok_or_else(not_open)?;
    if let Some((c, _)) = company_of(&mut tx, shop, customer).await? {
        if c.status == "on_hold" {
            return Err(
                Error::conflict("the company's account is on hold").with_code("company_on_hold")
            );
        }
        if let Some(limit) = c.credit_limit_minor {
            let draft = drafts::get(&mut tx, shop, taxes, &draft_id).await?;
            let owed = balance(&mut tx, shop, &c).await?.open_minor;
            if a.payment_method_id.is_none() && owed + draft.total_minor > limit {
                return Err(Error::conflict(format!(
                    "the order would take the company over its credit limit; {} is left",
                    (limit - owed).max(0)
                ))
                .with_code("credit_limit_exceeded"));
            }
        }
    }
    sqlx::query(
        "UPDATE quotes SET status = 'accepted', updated_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(shop.id())
    .bind(&r.id)
    .execute(&mut *tx)
    .await?;
    let c = CompleteDraft {
        paid: false,
        payment_method_id: a.payment_method_id,
        return_url: a.return_url,
    };
    let done = drafts::complete_in(db, tx, shop, taxes, providers, &draft_id, c).await?;
    let mut tx = shop.begin(db).await?;
    sqlx::query("UPDATE quotes SET order_id = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(shop.id())
        .bind(&r.id)
        .bind(&done.order.id)
        .execute(&mut *tx)
        .await?;
    let quote = load_quote(&mut tx, shop, taxes, &r.id).await?;
    Scope::publish(shop, &mut tx, "quote.accepted", &quote).await?;
    tx.commit().await?;
    Ok(Accepted {
        quote,
        order_id: done.order.id,
        order_token: done.order_token,
        payment: done.payment,
    })
}

/// Accept the shop's offer: its order is placed. Without a payment method
/// it is due within the company's terms.
#[post(
    "/api/storefront/account/quotes/{id}/accept",
    tag = "storefront",
    status = 201
)]
async fn accept_my_quote(
    db: Db,
    signed_in: SignedIn,
    taxes: State<TaxProviders>,
    providers: State<Providers>,
    Path(id): Path<String>,
    Valid(Json(a)): Valid<Json<Acceptance>>,
) -> Result<(StatusCode, Json<Accepted>)> {
    let (tx, customer) = signed_in.begin(&db).await?;
    tx.commit().await?;
    let done = accept(&db, &signed_in.shop, &taxes, &providers, &customer, &id, a).await?;
    Ok((StatusCode::CREATED, Json(done)))
}

/// Declines customer `customer`'s quote `id` (asked or offered).
/// Publishes `quote.declined`.
///
/// # Errors
///
/// 404 for someone else's quote; 409 `quote_closed` once answered.
pub async fn decline(
    tx: &mut TenantTx,
    shop: &Shop,
    taxes: &TaxProviders,
    customer: &str,
    id: &str,
) -> Result<Quote> {
    let r = mine(tx, shop, customer, id).await?;
    close(tx, shop, taxes, &r, None).await
}

/// Decline the shop's offer, or take back the request.
#[post("/api/storefront/account/quotes/{id}/decline", tag = "storefront")]
async fn decline_my_quote(
    db: Db,
    signed_in: SignedIn,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
) -> Result<Json<Quote>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let q = decline(&mut tx, &signed_in.shop, &taxes, &customer, &id).await?;
    tx.commit().await?;
    Ok(Json(q))
}

/// The lines of a quick order: `SKU quantity` (or `SKU,quantity`) per
/// line; a line without a quantity is one. Blank lines and lines starting
/// with `#` are skipped.
///
/// ```
/// let lines = soat_b2b::parse_quick_order("TEE-M 3\nMUG,2\n\nBOOK").unwrap();
/// assert_eq!(lines, vec![("TEE-M".into(), 3), ("MUG".into(), 2), ("BOOK".into(), 1)]);
/// ```
///
/// # Errors
///
/// The number of the first line that is not one, with what is wrong.
pub fn parse_quick_order(text: &str) -> std::result::Result<Vec<(String, i64)>, (usize, String)> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line
            .split(|c: char| c == ',' || c == ';' || c == '\t' || c.is_whitespace())
            .filter(|p| !p.is_empty());
        let sku = parts.next().unwrap_or_default().to_owned();
        let quantity = match parts.next() {
            Some(q) => q
                .parse::<i64>()
                .ok()
                .filter(|q| (1..=10_000).contains(q))
                .ok_or((n + 1, format!("{q} is not a quantity from 1 to 10000")))?,
            None => 1,
        };
        if parts.next().is_some() {
            return Err((n + 1, "a line is a SKU and a quantity".to_owned()));
        }
        match out.iter_mut().find(|(s, _)| s.eq_ignore_ascii_case(&sku)) {
            Some(existing) => existing.1 = (existing.1 + quantity).min(10_000),
            None => out.push((sku, quantity)),
        }
    }
    if out.len() > MAX_LINES {
        return Err((0, format!("a quick order has at most {MAX_LINES} lines")));
    }
    Ok(out)
}

/// The item with SKU `sku` (a variant's or a product's), sold today.
///
/// # Errors
///
/// Database errors.
pub async fn item_by_sku(
    conn: &mut PgConnection,
    scope: &impl Scope,
    sku: &str,
) -> Result<Option<(String, String)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT v.id, v.product_id FROM product_variants v JOIN products p ON p.id = v.product_id \
         WHERE v.tenant_id = $1 AND lower(v.sku) = lower($2) AND p.status = 'active' \
         UNION ALL \
         SELECT p.id, p.id FROM products p WHERE p.tenant_id = $1 AND lower(p.sku) = lower($2) \
         AND p.status = 'active' AND NOT EXISTS \
         (SELECT 1 FROM product_variants v WHERE v.tenant_id = p.tenant_id AND v.product_id = p.id) \
         LIMIT 1",
    )
    .bind(scope.tenant_id())
    .bind(sku)
    .fetch_optional(conn)
    .await?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_orders_parse() {
        assert_eq!(
            parse_quick_order("# my list\nA 2\na 3\nB;4\nC\t5\n"),
            Ok(vec![("A".into(), 5), ("B".into(), 4), ("C".into(), 5)])
        );
        assert_eq!(parse_quick_order("A x").unwrap_err().0, 1);
        assert_eq!(parse_quick_order("\nA 0").unwrap_err().0, 2);
        assert_eq!(parse_quick_order("A 1 2").unwrap_err().0, 1);
        assert_eq!(parse_quick_order("").unwrap(), vec![]);
    }

    #[test]
    fn tags_and_vat_numbers_are_tidied() {
        assert_eq!(price_tag(Some("  Wholesale ")), Some("wholesale".into()));
        assert_eq!(price_tag(Some(" ")), None);
        assert_eq!(vat(Some("de 123 456 789")), Some("DE123456789".into()));
    }
}
