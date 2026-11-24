//! Draft orders: staff put an order together for a buyer (a phone or
//! email order, a quote), with catalog items at their prices or prices of
//! their own, shipping, a discount and a note. The draft shows totals with
//! tax once it has an address; completing it places the order, holds the
//! stock and either records it as paid or starts a payment with one of the
//! shop's methods.

use serde_json::Value;
use soat_core::discount::allocate;
use soat_core::{id_kind, Currency};
use soat_orders::{
    NewOrder, NewOrderLine, NewShipping, Order, PostalAddress, ORDERS_READ, ORDERS_WRITE,
};
use soat_payments::{Payment, Providers, StartPayment};
use soat_tax::QuoteInput;
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{checked, hold};

id_kind!(DraftOrderId, DraftOrderKind, "dft");

/// Most lines on a draft.
const MAX_LINES: usize = 100;

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_drafts,
        create_draft,
        get_draft,
        update_draft,
        delete_draft,
        complete_draft,
    ))
}

/// A line of a draft, as given.
#[model]
pub struct DraftLineInput {
    /// A product without variants, or a variant.
    pub item_id: String,
    #[validate(range(min = 1, max = 10000))]
    pub quantity: i64,
    /// A price of your own, per unit; the catalog's when left out.
    #[validate(range(min = 0))]
    pub unit_price_minor: Option<i64>,
}

/// A draft order to create, or what replaces one.
#[model]
pub struct DraftInput {
    pub customer_id: Option<String>,
    #[validate(email, length(max = 254))]
    pub email: Option<String>,
    pub currency: String,
    #[serde(default)]
    pub lines: Vec<DraftLineInput>,
    pub shipping_address: Option<PostalAddress>,
    /// Defaults to the shipping address.
    pub billing_address: Option<PostalAddress>,
    #[validate(length(max = 20))]
    pub vat_number: Option<String>,
    /// Shipping as the staff member sets it, e.g. `Courier`; none when
    /// left out.
    #[validate(length(max = 200))]
    pub shipping_name: Option<String>,
    #[validate(range(min = 0))]
    pub shipping_price_minor: Option<i64>,
    /// Taken off the goods, shared over the lines.
    #[validate(range(min = 0))]
    pub discount_minor: Option<i64>,
    #[validate(length(max = 2000))]
    pub note: Option<String>,
}

/// A line of a draft, worked out.
#[model]
pub struct DraftLine {
    pub item_id: String,
    pub sku: String,
    pub title: String,
    pub quantity: i64,
    pub unit_price_minor: i64,
    /// Whether the price is the staff member's own.
    pub custom_price: bool,
    pub discount_minor: i64,
    pub tax_rate_bps: i64,
    pub tax_minor: i64,
}

/// A draft order with its totals.
#[model]
pub struct DraftOrder {
    /// e.g. `dft_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// `open`, or `completed` once its order is placed.
    pub status: String,
    pub order_id: Option<String>,
    pub customer_id: Option<String>,
    pub email: String,
    pub currency: String,
    pub lines: Vec<DraftLine>,
    pub shipping_address: Option<PostalAddress>,
    pub billing_address: Option<PostalAddress>,
    pub vat_number: Option<String>,
    pub shipping_name: Option<String>,
    pub shipping_price_minor: i64,
    pub subtotal_minor: i64,
    pub discount_minor: i64,
    /// Tax, once there is a shipping address.
    pub tax_minor: i64,
    pub total_minor: i64,
    pub prices_include_tax: bool,
    pub note: String,
    /// Lines whose item is gone or in another currency.
    pub issues: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    status: String,
    order_id: Option<String>,
    customer_id: Option<String>,
    email: String,
    currency: String,
    lines: SqlJson<Vec<StoredLine>>,
    shipping_address: Option<SqlJson<Value>>,
    billing_address: Option<SqlJson<Value>>,
    vat_number: Option<String>,
    shipping_name: Option<String>,
    shipping_price_minor: i64,
    discount_minor: i64,
    note: String,
    created_at: String,
    updated_at: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct StoredLine {
    item_id: String,
    quantity: i64,
    unit_price_minor: Option<i64>,
}

const COLUMNS: &str = "id, status, order_id, customer_id, email, currency, lines, \
     shipping_address, billing_address, vat_number, shipping_name, shipping_price_minor, \
     discount_minor, note, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_draft_order")
}

fn address(v: &Option<SqlJson<Value>>) -> Option<PostalAddress> {
    v.as_ref()
        .and_then(|j| serde_json::from_value(j.0.clone()).ok())
}

/// A draft worked out with today's catalog and tax, and the order it
/// would place.
async fn work_out(
    conn: &mut PgConnection,
    scope: &impl Scope,
    r: Row,
) -> Result<(DraftOrder, NewOrder)> {
    let currency = Currency::parse(&r.currency).map_err(|e| Error::internal(e.to_string()))?;
    let mut lines: Vec<NewOrderLine> = Vec::new();
    let mut custom = Vec::new();
    let mut issues = Vec::new();
    for l in &r.lines.0 {
        let Some(item) = soat_catalog::stock_item(conn, scope, &l.item_id)
            .await
            .ok()
            .flatten()
        else {
            issues.push(format!("{} is not an item of the shop", l.item_id));
            continue;
        };
        if l.unit_price_minor.is_none() && item.currency != r.currency {
            issues.push(format!("{} is priced in {}", item.sku, item.currency));
            continue;
        }
        custom.push(l.unit_price_minor.is_some());
        lines.push(NewOrderLine {
            item_id: item.id,
            product_id: Some(item.product_id),
            sku: item.sku,
            title: item.title,
            quantity: l.quantity,
            unit_price_minor: l.unit_price_minor.unwrap_or(item.price_minor),
            tax_class: item.tax_class,
            tax_rate_bps: 0,
            tax_minor: 0,
            discount_minor: 0,
        });
    }
    let worth: Vec<i64> = lines
        .iter()
        .map(|l| l.unit_price_minor.saturating_mul(l.quantity))
        .collect();
    for (line, share) in lines.iter_mut().zip(allocate(r.discount_minor, &worth)) {
        line.discount_minor = share;
    }
    let shipping_address = address(&r.shipping_address);
    let billing_address = address(&r.billing_address).or_else(|| shipping_address.clone());
    let mut shipping = r.shipping_name.as_ref().map(|name| NewShipping {
        rate_id: String::new(),
        name: name.clone(),
        price_minor: r.shipping_price_minor,
        discount_minor: 0,
        tax_minor: 0,
        tax_rate_bps: 0,
    });
    let mut tax = None;
    if let Some(addr) = &shipping_address {
        let mut amounts: Vec<(String, i64)> = lines
            .iter()
            .map(|l| {
                (
                    l.tax_class.clone(),
                    l.unit_price_minor * l.quantity - l.discount_minor,
                )
            })
            .collect();
        if let Some(s) = &shipping {
            amounts.push(("standard".to_owned(), s.price_minor));
        }
        let quote = soat_tax::quote(
            conn,
            scope,
            &QuoteInput {
                country: addr.country.clone(),
                vat_number: r.vat_number.clone(),
                lines: amounts,
            },
        )
        .await?;
        for (line, taxed) in lines.iter_mut().zip(&quote.lines) {
            line.tax_rate_bps = taxed.rate_bps;
            line.tax_minor = taxed.tax_minor;
        }
        if let (Some(s), Some(taxed)) = (shipping.as_mut(), quote.lines.get(lines.len())) {
            s.tax_minor = taxed.tax_minor;
            s.tax_rate_bps = taxed.rate_bps;
        }
        tax = Some(quote);
    }
    let prices_include_tax = match &tax {
        Some(q) => q.prices_include_tax,
        None => soat_tax::settings(conn, scope).await?.prices_include_tax,
    };
    let order = NewOrder {
        cart_id: None,
        customer_id: r.customer_id.clone(),
        email: r.email.clone(),
        currency,
        lines: lines.clone(),
        shipping,
        shipping_address: shipping_address.clone(),
        billing_address: billing_address.clone(),
        prices_include_tax,
        tax_country: tax.as_ref().map(|t| t.tax_country.clone()),
        reverse_charge: tax.as_ref().is_some_and(|t| t.reverse_charge),
        vat_number: r.vat_number.clone(),
        note: r.note.clone(),
        discount_code: None,
    };
    let totals = if order.lines.is_empty() {
        soat_orders::Totals {
            subtotal: 0,
            shipping: 0,
            tax: 0,
            discount: 0,
            total: 0,
        }
    } else {
        soat_orders::totals(&order)?
    };
    let draft = DraftOrder {
        id: r.id,
        status: r.status,
        order_id: r.order_id,
        customer_id: r.customer_id,
        email: r.email,
        currency: r.currency,
        lines: lines
            .iter()
            .zip(custom)
            .map(|(l, custom_price)| DraftLine {
                item_id: l.item_id.clone(),
                sku: l.sku.clone(),
                title: l.title.clone(),
                quantity: l.quantity,
                unit_price_minor: l.unit_price_minor,
                custom_price,
                discount_minor: l.discount_minor,
                tax_rate_bps: l.tax_rate_bps,
                tax_minor: l.tax_minor,
            })
            .collect(),
        shipping_address,
        billing_address,
        vat_number: r.vat_number,
        shipping_name: r.shipping_name,
        shipping_price_minor: r.shipping_price_minor,
        subtotal_minor: totals.subtotal,
        discount_minor: totals.discount,
        tax_minor: totals.tax,
        total_minor: totals.total,
        prices_include_tax,
        note: r.note,
        issues,
        created_at: r.created_at,
        updated_at: r.updated_at,
    };
    Ok((draft, order))
}

async fn row(conn: &mut PgConnection, scope: &impl Scope, id: &str, lock: bool) -> Result<Row> {
    let id = DraftOrderId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLUMNS} FROM draft_orders WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)
}

async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<DraftOrder> {
    let r = row(conn, scope, id, false).await?;
    Ok(work_out(conn, scope, r).await?.0)
}

async fn write(
    tx: &mut TenantTx,
    tenant: &Tenant,
    id: &str,
    d: &DraftInput,
    insert: bool,
) -> Result<Option<String>> {
    let currency = Currency::parse(&d.currency).map_err(|e| invalid(e.to_string()))?;
    if d.lines.len() > MAX_LINES {
        return Err(invalid(format!("a draft has at most {MAX_LINES} lines")));
    }
    for a in [&d.shipping_address, &d.billing_address]
        .into_iter()
        .flatten()
    {
        checked(a)?;
    }
    if let Some(v) = d.vat_number.as_deref().filter(|v| !v.trim().is_empty()) {
        if soat_core::tax::vat_number_country(v).is_none() {
            return Err(Error::unprocessable(format!("{v} is not an EU VAT number"))
                .with_code("invalid_vat_number"));
        }
    }
    let customer = d
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let mut email = d.email.as_deref().unwrap_or("").trim().to_lowercase();
    if let Some(c) = customer {
        let found = soat_customers::load(tx, tenant, c)
            .await
            .map_err(|_| invalid("the shop has no such customer"))?;
        if email.is_empty() {
            email = found.email.clone();
        }
    }
    for l in &d.lines {
        if !(1..=10_000).contains(&l.quantity) {
            return Err(invalid("a quantity is 1 to 10000"));
        }
        if l.unit_price_minor.is_some_and(|p| p < 0) {
            return Err(invalid("a price is not negative"));
        }
        if soat_catalog::stock_item(tx, tenant, &l.item_id)
            .await?
            .is_none()
        {
            return Err(invalid(format!("{} is not an item of the shop", l.item_id)));
        }
    }
    let lines: Vec<StoredLine> = d
        .lines
        .iter()
        .map(|l| StoredLine {
            item_id: l.item_id.clone(),
            quantity: l.quantity,
            unit_price_minor: l.unit_price_minor,
        })
        .collect();
    let json = |a: &Option<PostalAddress>| {
        a.as_ref()
            .map(|a| SqlJson(serde_json::to_value(a).unwrap_or(Value::Null)))
    };
    let sql = if insert {
        "INSERT INTO draft_orders (id, tenant_id, customer_id, email, currency, lines, \
         shipping_address, billing_address, vat_number, shipping_name, shipping_price_minor, \
         discount_minor, note) VALUES ($2, $1, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
         RETURNING id"
    } else {
        "UPDATE draft_orders SET customer_id = $3, email = $4, currency = $5, lines = $6, \
         shipping_address = $7, billing_address = $8, vat_number = $9, shipping_name = $10, \
         shipping_price_minor = $11, discount_minor = $12, note = $13, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND status = 'open' RETURNING id"
    };
    let done: Option<(String,)> = sqlx::query_as(sql)
        .bind(tenant.id())
        .bind(id)
        .bind(customer)
        .bind(&email)
        .bind(currency.as_str())
        .bind(SqlJson(&lines))
        .bind(json(&d.shipping_address))
        .bind(json(&d.billing_address))
        .bind(
            d.vat_number
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
        )
        .bind(
            d.shipping_name
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
        )
        .bind(d.shipping_price_minor.unwrap_or(0))
        .bind(d.discount_minor.unwrap_or(0))
        .bind(d.note.as_deref().unwrap_or("").trim())
        .fetch_optional(&mut **tx)
        .await?;
    Ok(done.map(|(id,)| id))
}

/// The shop's draft orders, newest first (`orders:read`). Filter with
/// `?status=open` or `completed`; `?q=` searches emails.
#[get("/api/draft-orders", tag = "orders")]
async fn list_drafts(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<DraftOrder>>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let page: Paged<Row> = TenantListing::new("draft_orders", COLUMNS)
        .sortable(["id"])
        .default_sort("-id")
        .search(["email"])
        .filter_text("status")
        .fetch(&mut tx, &params)
        .await?;
    let mut items = Vec::with_capacity(page.items.len());
    for r in page.items {
        items.push(work_out(&mut tx, &tenant, r).await?.0);
    }
    tx.commit().await?;
    Ok(Json(Paged {
        items,
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// Start a draft order (`orders:write`). Publishes `draft_order.created`.
#[post("/api/draft-orders", tag = "orders", status = 201)]
async fn create_draft(
    db: Db,
    tenant: Tenant,
    Valid(Json(d)): Valid<Json<DraftInput>>,
) -> Result<(StatusCode, Json<DraftOrder>)> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let id = write(&mut tx, &tenant, DraftOrderId::new().as_str(), &d, true)
        .await?
        .ok_or_else(|| Error::internal("insert returned nothing"))?;
    let draft = load(&mut tx, &tenant, &id).await?;
    tenant
        .publish(&mut tx, "draft_order.created", &draft)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(draft)))
}

/// One draft order with today's totals (`orders:read`).
#[get("/api/draft-orders/{id}", tag = "orders")]
async fn get_draft(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<DraftOrder>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let draft = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(draft))
}

/// Replace an open draft order (`orders:write`); a completed one is a 409
/// `draft_completed`. Publishes `draft_order.updated`.
#[put("/api/draft-orders/{id}", tag = "orders")]
async fn update_draft(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(d)): Valid<Json<DraftInput>>,
) -> Result<Json<DraftOrder>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let current = row(&mut tx, &tenant, &id, true).await?;
    if current.status != "open" {
        return Err(completed());
    }
    write(&mut tx, &tenant, &current.id, &d, false).await?;
    let draft = load(&mut tx, &tenant, &current.id).await?;
    tenant
        .publish(&mut tx, "draft_order.updated", &draft)
        .await?;
    tx.commit().await?;
    Ok(Json(draft))
}

fn completed() -> Error {
    Error::conflict("this draft order has been placed").with_code("draft_completed")
}

/// Delete a draft order (`orders:write`); its order, if placed, stays.
/// Publishes `draft_order.deleted`.
#[delete("/api/draft-orders/{id}", tag = "orders", status = 204)]
async fn delete_draft(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let r = row(&mut tx, &tenant, &id, true).await?;
    sqlx::query("DELETE FROM draft_orders WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&r.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "draft_order.deleted",
            &serde_json::json!({ "id": r.id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// How a completed draft is paid.
#[model]
pub struct CompleteDraft {
    /// The buyer has paid already (cash, a transfer that arrived): the
    /// order is recorded as paid.
    #[serde(default)]
    pub paid: bool,
    /// Otherwise, start a payment with one of the shop's methods (e.g. an
    /// invoice with instructions); none leaves the order unpaid.
    pub payment_method_id: Option<String>,
    #[validate(url)]
    pub return_url: Option<String>,
}

/// The order placed from a draft, and its payment.
#[model]
pub struct CompletedDraft {
    pub draft_order: DraftOrder,
    pub order: Order,
    /// Send as `x-soat-order-token` to see the order as the buyer; shown
    /// once.
    pub order_token: String,
    pub payment: Option<Payment>,
}

/// Place the draft's order (`orders:write`): it needs lines, an email and
/// a shipping address (422 `draft_incomplete`), holds the stock (409
/// `insufficient_stock`), and is recorded as paid or gets a payment
/// started. Publishes `order.placed` and `draft_order.completed`.
#[post("/api/draft-orders/{id}/complete", tag = "orders", status = 201)]
async fn complete_draft(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CompleteDraft>>,
) -> Result<(StatusCode, Json<CompletedDraft>)> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let r = row(&mut tx, &tenant, &id, true).await?;
    if r.status != "open" {
        return Err(completed());
    }
    let (_, order) = work_out(&mut tx, &tenant, r).await?;
    let draft_id = DraftOrderId::parse(&id).map_err(|_| Error::not_found())?;
    let mut missing = Vec::new();
    if order.lines.is_empty() {
        missing.push("lines");
    }
    if order.email.is_empty() {
        missing.push("email");
    }
    if order.shipping_address.is_none() {
        missing.push("shipping_address");
    }
    if !missing.is_empty() {
        return Err(Error::unprocessable(format!(
            "the draft order still needs: {}",
            missing.join(", ")
        ))
        .with_code("draft_incomplete")
        .with_details(&missing));
    }
    let method = match (c.paid, c.payment_method_id.as_deref()) {
        (false, Some(m)) => Some(
            soat_payments::active_method(&mut tx, &tenant, m)
                .await?
                .ok_or_else(|| {
                    Error::unprocessable("choose one of the shop's payment methods")
                        .with_code("unknown_payment_method")
                })?,
        ),
        _ => None,
    };
    let (mut placed, token) = soat_orders::place(&mut tx, &tenant, &order).await?;
    hold(&mut tx, &tenant, &order.lines, &placed.id, true).await?;
    if c.paid && placed.total_minor > 0 {
        soat_payments::record(
            &mut tx,
            &tenant,
            &placed.id,
            "manual",
            &placed.id,
            placed.total_minor,
            placed.currency.as_str(),
        )
        .await?;
        soat_orders::sync_payments(&mut tx, &tenant, &placed.id).await?;
        placed = soat_orders::load(&mut tx, &tenant, &placed.id)
            .await?
            .ok_or_else(|| Error::internal("order vanished"))?;
    }
    sqlx::query(
        "UPDATE draft_orders SET status = 'completed', order_id = $3, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(draft_id.as_str())
    .bind(&placed.id)
    .execute(&mut *tx)
    .await?;
    let draft = load(&mut tx, &tenant, draft_id.as_str()).await?;
    tenant
        .publish(&mut tx, "draft_order.completed", &draft)
        .await?;
    tx.commit().await?;
    let payment = match method.filter(|_| placed.total_minor > 0) {
        Some(m) => {
            let req = StartPayment {
                method_id: m.id,
                reference: placed.id.clone(),
                amount_minor: placed.total_minor,
                currency: Currency::parse(&placed.currency)
                    .map_err(|e| Error::internal(e.to_string()))?,
                return_url: c.return_url,
            };
            Some(soat_payments::start(&db, &providers, &tenant, &req).await?)
        }
        None => None,
    };
    Ok((
        StatusCode::CREATED,
        Json(CompletedDraft {
            draft_order: draft,
            order: placed,
            order_token: token,
            payment,
        }),
    ))
}
