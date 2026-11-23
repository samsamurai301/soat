//! Orders.
//!
//! Checkout (C9) places an order with [`place`]: it copies in the lines
//! with their prices and tax, the shipping choice and the addresses, so an
//! order never changes when the catalog does, and numbers it per shop
//! (1001, 1002, ...). A guest sees their order with the token returned
//! once; a signed-in customer sees theirs in their account.
//!
//! An order's `financial_status` follows its payments (whose reference is
//! the order id): a subscriber recomputes it on every `payment.updated`.
//! Canceling releases the stock held for it. [`fulfillments`] ship its
//! lines and take them out of stock; [`returns`] bring units back and
//! refund them.

pub mod fulfillments;
pub mod report;
mod returns;
mod routes;

pub use routes::{customer_order, customer_orders, OrderSummary, OrderToken};

use std::sync::Arc;

use serde_json::Value;
use soat_core::order::{financial_status, PaidPart};
use soat_core::{id_kind, Currency, Permission};
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_tenancy::{Scope, TenantTx};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::sqlx;
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::Resources;

id_kind!(OrderId, OrderKind, "ord");
id_kind!(OrderLineId, OrderLineKind, "ol");

/// The first order number of a shop.
pub const FIRST_NUMBER: i64 = 1001;
/// The header holding a guest's order token.
pub const TOKEN_HEADER: &str = "x-soat-order-token";

/// See orders.
pub const ORDERS_READ: Permission = Permission::new("orders:read", "See orders").for_staff();
/// Change and cancel orders.
pub const ORDERS_WRITE: Permission =
    Permission::new("orders:write", "Change and cancel orders").for_staff();

/// The orders module.
pub struct Orders;

impl Module for Orders {
    fn name(&self) -> &'static str {
        "orders"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "customers", "inventory", "payments"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[ORDERS_READ, ORDERS_WRITE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(PaymentFollower)]
    }

    fn mount(&self, app: App) -> App {
        app.plugin(routes::mount)
            .plugin(fulfillments::mount)
            .plugin(returns::mount)
            .plugin(report::mount)
    }
}

/// A postal address as it was given for an order.
#[model]
#[derive(Default)]
pub struct PostalAddress {
    #[validate(length(max = 100))]
    #[serde(default)]
    pub first_name: String,
    #[validate(length(max = 100))]
    #[serde(default)]
    pub last_name: String,
    #[validate(length(max = 200))]
    #[serde(default)]
    pub company: String,
    #[validate(length(min = 1, max = 200))]
    pub line1: String,
    #[validate(length(max = 200))]
    #[serde(default)]
    pub line2: String,
    #[validate(length(max = 20))]
    #[serde(default)]
    pub postal_code: String,
    #[validate(length(min = 1, max = 100))]
    pub city: String,
    #[validate(length(max = 100))]
    #[serde(default)]
    pub region: String,
    /// ISO 3166-1 alpha-2, e.g. `DE`.
    pub country: String,
    #[validate(length(max = 40))]
    #[serde(default)]
    pub phone: String,
}

/// A placed order.
#[model]
pub struct Order {
    /// e.g. `ord_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Per shop, from 1001.
    pub number: i64,
    pub cart_id: Option<String>,
    pub customer_id: Option<String>,
    pub email: String,
    pub currency: String,
    /// `open` or `canceled`.
    pub status: String,
    /// `pending`, `authorized`, `partially_paid`, `paid`,
    /// `partially_refunded` or `refunded`.
    pub financial_status: String,
    /// `unfulfilled`, `partially_fulfilled` or `fulfilled`.
    pub fulfillment_status: String,
    pub lines: Vec<OrderLine>,
    /// Shipments, oldest first.
    pub fulfillments: Vec<fulfillments::Fulfillment>,
    /// Sum of the lines.
    pub subtotal_minor: i64,
    pub shipping_minor: i64,
    /// All tax, of the lines and of shipping.
    pub tax_minor: i64,
    /// Taken off the lines and shipping.
    pub discount_minor: i64,
    /// The discount code used.
    pub discount_code: Option<String>,
    /// What the buyer pays.
    pub total_minor: i64,
    /// Whether the line prices hold the tax (then it is not added on top).
    pub prices_include_tax: bool,
    pub tax_country: Option<String>,
    pub reverse_charge: bool,
    pub vat_number: Option<String>,
    pub shipping_address: Option<PostalAddress>,
    pub billing_address: Option<PostalAddress>,
    pub shipping_rate_id: Option<String>,
    pub shipping_name: Option<String>,
    pub shipping_tax_minor: i64,
    /// The tax rate on shipping, in basis points.
    pub shipping_tax_rate_bps: i64,
    /// What a discount code took off shipping.
    pub shipping_discount_minor: i64,
    pub note: String,
    pub cancel_reason: Option<String>,
    /// RFC 3339, UTC.
    pub placed_at: String,
    pub canceled_at: Option<String>,
}

/// A line of an order, as bought.
#[model]
#[derive(sqlx::FromRow)]
pub struct OrderLine {
    pub id: String,
    pub item_id: String,
    pub product_id: Option<String>,
    pub sku: String,
    pub title: String,
    pub quantity: i32,
    pub unit_price_minor: i64,
    pub total_minor: i64,
    pub tax_class: String,
    pub tax_rate_bps: i64,
    pub tax_minor: i64,
    /// The line's share of the order's discount.
    pub discount_minor: i64,
    /// Units shipped (not counting canceled shipments).
    pub fulfilled_quantity: i64,
}

/// A line to place.
#[derive(Debug, Clone)]
pub struct NewOrderLine {
    pub item_id: String,
    pub product_id: Option<String>,
    pub sku: String,
    pub title: String,
    pub quantity: i64,
    pub unit_price_minor: i64,
    pub tax_class: String,
    pub tax_rate_bps: i64,
    /// Tax on the line after its discount.
    pub tax_minor: i64,
    /// The line's share of the discount, taken off its total.
    pub discount_minor: i64,
}

/// The chosen shipping.
#[derive(Debug, Clone)]
pub struct NewShipping {
    pub rate_id: String,
    pub name: String,
    pub price_minor: i64,
    /// Taken off the price (free shipping codes).
    pub discount_minor: i64,
    /// Tax on the price after the discount.
    pub tax_minor: i64,
    pub tax_rate_bps: i64,
}

/// An order to place.
#[derive(Debug, Clone)]
pub struct NewOrder {
    pub cart_id: Option<String>,
    pub customer_id: Option<String>,
    pub email: String,
    pub currency: Currency,
    pub lines: Vec<NewOrderLine>,
    pub shipping: Option<NewShipping>,
    pub shipping_address: Option<PostalAddress>,
    pub billing_address: Option<PostalAddress>,
    pub prices_include_tax: bool,
    pub tax_country: Option<String>,
    pub reverse_charge: bool,
    pub vat_number: Option<String>,
    pub note: String,
    /// The discount code used, as entered after normalizing.
    pub discount_code: Option<String>,
}

/// The totals of an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// Sum of the lines before discounts.
    pub subtotal: i64,
    /// Shipping before its discount.
    pub shipping: i64,
    pub tax: i64,
    /// Of the lines and of shipping.
    pub discount: i64,
    pub total: i64,
}

/// The totals of an order: subtotal plus shipping, less discounts, with tax
/// added on top unless prices include it.
///
/// # Errors
///
/// `422` when an amount overflows or a discount is larger than what it
/// discounts.
pub fn totals(o: &NewOrder) -> Result<Totals> {
    let too_large = || Error::unprocessable("the order total is too large");
    let bad_discount = || Error::unprocessable("a discount is larger than its amount");
    let mut t = Totals {
        subtotal: 0,
        shipping: 0,
        tax: 0,
        discount: 0,
        total: 0,
    };
    for l in &o.lines {
        let line = l
            .unit_price_minor
            .checked_mul(l.quantity)
            .ok_or_else(too_large)?;
        if l.discount_minor < 0 || l.discount_minor > line {
            return Err(bad_discount());
        }
        t.subtotal = t.subtotal.checked_add(line).ok_or_else(too_large)?;
        t.discount = t
            .discount
            .checked_add(l.discount_minor)
            .ok_or_else(too_large)?;
        t.tax = t.tax.checked_add(l.tax_minor).ok_or_else(too_large)?;
    }
    if let Some(s) = &o.shipping {
        if s.discount_minor < 0 || s.discount_minor > s.price_minor {
            return Err(bad_discount());
        }
        t.shipping = s.price_minor;
        t.discount = t
            .discount
            .checked_add(s.discount_minor)
            .ok_or_else(too_large)?;
        t.tax = t.tax.checked_add(s.tax_minor).ok_or_else(too_large)?;
    }
    t.total = t.subtotal.checked_add(t.shipping).ok_or_else(too_large)? - t.discount;
    if !o.prices_include_tax {
        t.total = t.total.checked_add(t.tax).ok_or_else(too_large)?;
    }
    Ok(t)
}

/// Places an order in the shop's transaction: numbers it, copies
/// everything in and publishes `order.placed`. Returns it with the token a
/// guest uses to see it (`x-soat-order-token`), shown once.
pub async fn place(tx: &mut TenantTx, scope: &impl Scope, o: &NewOrder) -> Result<(Order, String)> {
    if o.lines.is_empty() {
        return Err(Error::unprocessable("an order needs lines").with_code("empty_order"));
    }
    let t = totals(o)?;
    let (number,): (i64,) = sqlx::query_as(
        "INSERT INTO order_counters (tenant_id, last) VALUES ($1, $2) \
         ON CONFLICT (tenant_id) DO UPDATE SET last = order_counters.last + 1 RETURNING last",
    )
    .bind(scope.tenant_id())
    .bind(FIRST_NUMBER)
    .fetch_one(&mut **tx)
    .await?;
    let id = OrderId::new();
    let token = format!("soat_order_{}", new_token()?);
    let json = |a: &Option<PostalAddress>| {
        a.as_ref()
            .map(|a| SqlJson(serde_json::to_value(a).unwrap_or(Value::Null)))
    };
    sqlx::query(
        "INSERT INTO orders (id, tenant_id, number, token_hash, cart_id, customer_id, email, \
         currency, subtotal_minor, shipping_minor, tax_minor, total_minor, prices_include_tax, \
         tax_country, reverse_charge, vat_number, shipping_address, billing_address, \
         shipping_rate_id, shipping_name, shipping_tax_minor, note, discount_minor, \
         discount_code, shipping_tax_rate_bps, shipping_discount_minor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
         $18, $19, $20, $21, $22, $23, $24, $25, $26)",
    )
    .bind(id.as_str())
    .bind(scope.tenant_id())
    .bind(number)
    .bind(hash_token(&token))
    .bind(&o.cart_id)
    .bind(&o.customer_id)
    .bind(o.email.trim().to_lowercase())
    .bind(o.currency.as_str())
    .bind(t.subtotal)
    .bind(t.shipping)
    .bind(t.tax)
    .bind(t.total)
    .bind(o.prices_include_tax)
    .bind(&o.tax_country)
    .bind(o.reverse_charge)
    .bind(&o.vat_number)
    .bind(json(&o.shipping_address))
    .bind(json(&o.billing_address))
    .bind(o.shipping.as_ref().map(|s| &s.rate_id))
    .bind(o.shipping.as_ref().map(|s| &s.name))
    .bind(o.shipping.as_ref().map_or(0, |s| s.tax_minor))
    .bind(o.note.trim())
    .bind(t.discount)
    .bind(&o.discount_code)
    .bind(o.shipping.as_ref().map_or(0, |s| s.tax_rate_bps))
    .bind(o.shipping.as_ref().map_or(0, |s| s.discount_minor))
    .execute(&mut **tx)
    .await?;
    for (position, l) in o.lines.iter().enumerate() {
        let quantity =
            i32::try_from(l.quantity).map_err(|_| Error::unprocessable("quantity too large"))?;
        sqlx::query(
            "INSERT INTO order_lines (id, tenant_id, order_id, position, item_id, product_id, \
             sku, title, quantity, unit_price_minor, total_minor, tax_class, tax_rate_bps, \
             tax_minor, discount_minor) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(OrderLineId::new().as_str())
        .bind(scope.tenant_id())
        .bind(id.as_str())
        .bind(position as i32)
        .bind(&l.item_id)
        .bind(&l.product_id)
        .bind(&l.sku)
        .bind(&l.title)
        .bind(quantity)
        .bind(l.unit_price_minor)
        .bind(l.unit_price_minor * l.quantity)
        .bind(&l.tax_class)
        .bind(l.tax_rate_bps)
        .bind(l.tax_minor)
        .bind(l.discount_minor)
        .execute(&mut **tx)
        .await?;
    }
    let order = load(tx, scope, id.as_str())
        .await?
        .ok_or_else(|| Error::internal("the order just placed is missing"))?;
    scope.publish(tx, "order.placed", &order).await?;
    Ok((order, token))
}

#[derive(sqlx::FromRow)]
struct OrderRow {
    id: String,
    number: i64,
    cart_id: Option<String>,
    customer_id: Option<String>,
    email: String,
    currency: String,
    status: String,
    financial_status: String,
    fulfillment_status: String,
    subtotal_minor: i64,
    shipping_minor: i64,
    tax_minor: i64,
    discount_minor: i64,
    discount_code: Option<String>,
    total_minor: i64,
    prices_include_tax: bool,
    tax_country: Option<String>,
    reverse_charge: bool,
    vat_number: Option<String>,
    shipping_address: Option<SqlJson<Value>>,
    billing_address: Option<SqlJson<Value>>,
    shipping_rate_id: Option<String>,
    shipping_name: Option<String>,
    shipping_tax_minor: i64,
    shipping_tax_rate_bps: i64,
    shipping_discount_minor: i64,
    note: String,
    cancel_reason: Option<String>,
    placed_at: String,
    canceled_at: Option<String>,
}

pub(crate) const ORDER_COLUMNS: &str = "id, number, cart_id, customer_id, email, currency, \
     status, financial_status, fulfillment_status, subtotal_minor, shipping_minor, tax_minor, \
     discount_minor, discount_code, total_minor, prices_include_tax, tax_country, reverse_charge, vat_number, \
     shipping_address, billing_address, shipping_rate_id, shipping_name, shipping_tax_minor, \
     shipping_tax_rate_bps, shipping_discount_minor, note, cancel_reason, \
     to_char(placed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS placed_at, \
     to_char(canceled_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS canceled_at";

fn address(v: Option<SqlJson<Value>>) -> Option<PostalAddress> {
    v.and_then(|j| serde_json::from_value(j.0).ok())
}

/// One order with its lines, in the shop's transaction.
pub async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Option<Order>> {
    let Ok(id) = OrderId::parse(id) else {
        return Ok(None);
    };
    let row = sqlx::query_as::<_, OrderRow>(&format!(
        "SELECT {ORDER_COLUMNS} FROM orders WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    let lines = sqlx::query_as::<_, OrderLine>(
        "SELECT l.id, l.item_id, l.product_id, l.sku, l.title, l.quantity, l.unit_price_minor, \
         l.total_minor, l.tax_class, l.tax_rate_bps, l.tax_minor, l.discount_minor, \
         COALESCE((SELECT SUM(fl.quantity) FROM fulfillment_lines fl \
                   JOIN fulfillments f ON f.id = fl.fulfillment_id \
                   WHERE fl.order_line_id = l.id AND f.status <> 'canceled'), 0)::BIGINT \
           AS fulfilled_quantity \
         FROM order_lines l WHERE l.tenant_id = $1 AND l.order_id = $2 ORDER BY l.position",
    )
    .bind(scope.tenant_id())
    .bind(&r.id)
    .fetch_all(&mut *conn)
    .await?;
    let fulfillments = fulfillments::of_order(conn, scope, &r.id).await?;
    Ok(Some(Order {
        id: r.id,
        number: r.number,
        cart_id: r.cart_id,
        customer_id: r.customer_id,
        email: r.email,
        currency: r.currency,
        status: r.status,
        financial_status: r.financial_status,
        fulfillment_status: r.fulfillment_status,
        lines,
        fulfillments,
        subtotal_minor: r.subtotal_minor,
        shipping_minor: r.shipping_minor,
        tax_minor: r.tax_minor,
        discount_minor: r.discount_minor,
        discount_code: r.discount_code,
        total_minor: r.total_minor,
        prices_include_tax: r.prices_include_tax,
        tax_country: r.tax_country,
        reverse_charge: r.reverse_charge,
        vat_number: r.vat_number,
        shipping_address: address(r.shipping_address),
        billing_address: address(r.billing_address),
        shipping_rate_id: r.shipping_rate_id,
        shipping_name: r.shipping_name,
        shipping_tax_minor: r.shipping_tax_minor,
        shipping_tax_rate_bps: r.shipping_tax_rate_bps,
        shipping_discount_minor: r.shipping_discount_minor,
        note: r.note,
        cancel_reason: r.cancel_reason,
        placed_at: r.placed_at,
        canceled_at: r.canceled_at,
    }))
}

/// The shop an event belongs to; the system acts.
struct EventScope(String);

impl Scope for EventScope {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// Recomputes an order's financial status from its payments, in the
/// shop's transaction. Publishes `order.paid` when it becomes paid and
/// `order.updated` on any other change.
pub async fn sync_payments(tx: &mut TenantTx, scope: &impl Scope, order_id: &str) -> Result<()> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT total_minor, financial_status FROM orders \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(scope.tenant_id())
    .bind(order_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((total, current)) = row else {
        return Ok(());
    };
    let payments = soat_payments::for_reference(tx, scope, order_id).await?;
    let parts: Vec<PaidPart> = payments
        .iter()
        .filter_map(|p| {
            Some(PaidPart {
                status: p.status.parse().ok()?,
                amount_minor: p.amount_minor,
                refunded_minor: p.refunded_minor,
            })
        })
        .collect();
    let next = financial_status(total, &parts);
    if next == current {
        return Ok(());
    }
    sqlx::query("UPDATE orders SET financial_status = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(scope.tenant_id())
        .bind(order_id)
        .bind(next)
        .execute(&mut **tx)
        .await?;
    let order = load(tx, scope, order_id)
        .await?
        .ok_or_else(|| Error::internal("order vanished"))?;
    let kind = if next == "paid" {
        "order.paid"
    } else {
        "order.updated"
    };
    scope.publish(tx, kind, &order).await?;
    Ok(())
}

/// Keeps orders' financial status in step with their payments.
struct PaymentFollower;

impl Subscriber for PaymentFollower {
    fn name(&self) -> &'static str {
        "orders"
    }

    fn wants(&self, kind: &str) -> bool {
        kind == "payment.updated"
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(reference) = event.payload["reference"].as_str() else {
                return Ok(());
            };
            if OrderId::parse(reference).is_err() {
                return Ok(());
            }
            let pool: PgPool = res.require()?;
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT set_config('soat.tenant', $1, true)")
                .bind(&event.tenant_id)
                .execute(&mut *tx)
                .await?;
            let scope = EventScope(event.tenant_id.clone());
            sync_payments(&mut tx, &scope, reference).await?;
            tx.commit().await?;
            Ok(())
        })
    }
}
