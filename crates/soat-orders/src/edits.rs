//! Order edits: staff change how many of a line the buyer gets, take a
//! line out, or add items to an open order. Lines keep their price and
//! tax rate; a line's discount shrinks with it. Stock held for the order
//! follows, the totals are worked out again, and the financial status
//! follows the new total: what the buyer still owes, or is owed back, is
//! in `balance_minor`.

use serde_json::json;
use soat_core::tax::tax_on;
use soat_tax::providers::TaxProviders;
use soat_tax::QuoteInput;
use soat_tenancy::Tenant;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{load, sync_payments, Order, OrderLineId, ORDERS_WRITE};

pub(crate) fn mount(app: App) -> App {
    app.mount(edit_order)
}

/// A line's new quantity; 0 takes it out.
#[model]
pub struct LineChange {
    pub line_id: String,
    #[validate(range(min = 0, max = 10000))]
    pub quantity: i64,
}

/// Units of an item to add, at its catalog price or one of your own.
#[model]
pub struct LineAdd {
    pub item_id: String,
    #[validate(range(min = 1, max = 10000))]
    pub quantity: i64,
    #[validate(range(min = 0))]
    pub unit_price_minor: Option<i64>,
}

/// Body of an order edit.
#[model]
pub struct OrderEdit {
    #[serde(default)]
    pub lines: Vec<LineChange>,
    #[serde(default)]
    pub add: Vec<LineAdd>,
    /// Why, for the order's history.
    #[validate(length(max = 500))]
    pub reason: Option<String>,
}

/// The order after an edit.
#[model]
pub struct EditedOrder {
    pub order: Order,
    /// The total before the edit.
    pub previous_total_minor: i64,
    /// What the buyer still owes (positive) or should get back
    /// (negative), against what was collected.
    pub balance_minor: i64,
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_order_edit")
}

/// Change an open order's lines (`orders:write`): new quantities (not
/// below what shipped; 0 takes an unshipped line out) and items added.
/// Stock is held or let go to match (409 `insufficient_stock`). A
/// canceled order is a 409 `order_closed`. Publishes `order.edited`, and
/// `order.updated` or `order.paid` when the financial status changes.
#[post("/api/orders/{id}/edit", tag = "orders")]
async fn edit_order(
    db: Db,
    tenant: Tenant,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
    Valid(Json(e)): Valid<Json<OrderEdit>>,
) -> Result<Json<EditedOrder>> {
    if e.lines.is_empty() && e.add.is_empty() {
        return Err(invalid("change a line or add one"));
    }
    if e.lines.iter().any(|l| !(0..=10_000).contains(&l.quantity))
        || e.add.iter().any(|a| !(1..=10_000).contains(&a.quantity))
        || e.add
            .iter()
            .any(|a| a.unit_price_minor.is_some_and(|p| p < 0))
    {
        return Err(invalid(
            "a quantity is 0 to 10000 (1 to 10000 to add), a price not negative",
        ));
    }
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    sqlx::query("SELECT 1 FROM orders WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant.id())
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let order = load(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    if order.status != "open" {
        return Err(Error::conflict("a canceled order cannot be edited").with_code("order_closed"));
    }
    let inclusive = order.prices_include_tax;
    let mut changes = Vec::new();
    for c in &e.lines {
        let line = order
            .lines
            .iter()
            .find(|l| l.id == c.line_id)
            .ok_or_else(|| invalid(format!("the order has no line {}", c.line_id)))?;
        let old = i64::from(line.quantity);
        if c.quantity == old {
            continue;
        }
        if c.quantity < line.fulfilled_quantity {
            return Err(invalid(format!(
                "{} units of {} have shipped already",
                line.fulfilled_quantity, line.sku
            )));
        }
        if c.quantity > old {
            soat_inventory::reservations::reserve(
                &mut tx,
                &tenant,
                &line.item_id,
                c.quantity - old,
                &order.id,
                None,
            )
            .await?;
        } else {
            soat_inventory::reservations::take(
                &mut tx,
                &tenant,
                &order.id,
                &line.item_id,
                old - c.quantity,
            )
            .await?;
        }
        if c.quantity == 0 {
            sqlx::query("DELETE FROM order_lines WHERE tenant_id = $1 AND id = $2")
                .bind(tenant.id())
                .bind(&line.id)
                .execute(&mut *tx)
                .await?;
        } else {
            let gross = line.unit_price_minor * c.quantity;
            let discount = line.discount_minor * c.quantity / old;
            let tax = tax_on(gross - discount, line.tax_rate_bps, inclusive);
            sqlx::query(
                "UPDATE order_lines SET quantity = $3, total_minor = $4, discount_minor = $5, \
                 tax_minor = $6 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant.id())
            .bind(&line.id)
            .bind(c.quantity as i32)
            .bind(gross)
            .bind(discount)
            .bind(tax)
            .execute(&mut *tx)
            .await?;
        }
        changes.push(json!({ "line_id": line.id, "sku": line.sku, "from": old, "to": c.quantity }));
    }
    for (position, a) in (order.lines.len() as i32..).zip(e.add.iter()) {
        let item = soat_catalog::stock_item(&mut tx, &tenant, &a.item_id)
            .await?
            .ok_or_else(|| invalid(format!("{} is not an item of the shop", a.item_id)))?;
        if a.unit_price_minor.is_none() && item.currency != order.currency {
            return Err(invalid(format!(
                "{} is priced in {}",
                item.sku, item.currency
            )));
        }
        let price = a.unit_price_minor.unwrap_or(item.price_minor);
        let gross = price * a.quantity;
        let taxed_at = order
            .shipping_address
            .as_ref()
            .or(order.billing_address.as_ref());
        let rate = match (&order.tax_country, taxed_at) {
            (Some(_), Some(addr)) => {
                let q = soat_tax::quote(
                    &mut tx,
                    &tenant,
                    &taxes,
                    &QuoteInput {
                        country: addr.country.clone(),
                        postal_code: Some(addr.postal_code.clone()).filter(|p| !p.is_empty()),
                        vat_number: order.vat_number.clone(),
                        lines: vec![(item.tax_class.clone(), gross)],
                    },
                )
                .await?;
                q.lines.first().map_or(0, |l| l.rate_bps)
            }
            _ => 0,
        };
        soat_inventory::reservations::reserve(
            &mut tx, &tenant, &item.id, a.quantity, &order.id, None,
        )
        .await?;
        sqlx::query(
            "INSERT INTO order_lines (id, tenant_id, order_id, position, item_id, product_id, \
             sku, title, quantity, unit_price_minor, total_minor, tax_class, tax_rate_bps, \
             tax_minor, discount_minor, requires_shipping) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 0, $15)",
        )
        .bind(OrderLineId::new().as_str())
        .bind(tenant.id())
        .bind(&order.id)
        .bind(position)
        .bind(&item.id)
        .bind(&item.product_id)
        .bind(&item.sku)
        .bind(&item.title)
        .bind(a.quantity as i32)
        .bind(price)
        .bind(gross)
        .bind(&item.tax_class)
        .bind(rate)
        .bind(tax_on(gross, rate, inclusive))
        .bind(item.requires_shipping)
        .execute(&mut *tx)
        .await?;
        changes.push(json!({ "item_id": item.id, "sku": item.sku, "added": a.quantity }));
    }
    if changes.is_empty() {
        return Err(invalid("nothing would change"));
    }
    let left: (i64,) =
        sqlx::query_as("SELECT count(*) FROM order_lines WHERE tenant_id = $1 AND order_id = $2")
            .bind(tenant.id())
            .bind(&order.id)
            .fetch_one(&mut *tx)
            .await?;
    if left.0 == 0 {
        return Err(invalid(
            "an order keeps at least one line; cancel it instead",
        ));
    }
    // Totals from the lines and the shipping as it was.
    sqlx::query(
        "UPDATE orders o SET subtotal_minor = l.subtotal, \
         discount_minor = l.discount + o.shipping_discount_minor, \
         tax_minor = l.tax + o.shipping_tax_minor, \
         total_minor = l.subtotal + o.shipping_minor - l.discount - o.shipping_discount_minor \
             + CASE WHEN o.prices_include_tax THEN 0 ELSE l.tax + o.shipping_tax_minor END \
         FROM (SELECT COALESCE(sum(total_minor), 0)::bigint AS subtotal, \
                      COALESCE(sum(discount_minor), 0)::bigint AS discount, \
                      COALESCE(sum(tax_minor), 0)::bigint AS tax \
               FROM order_lines WHERE tenant_id = $1 AND order_id = $2) l \
         WHERE o.tenant_id = $1 AND o.id = $2",
    )
    .bind(tenant.id())
    .bind(&order.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE orders SET fulfillment_status = CASE \
           WHEN NOT EXISTS (SELECT 1 FROM order_lines l WHERE l.tenant_id = $1 AND l.order_id = $2 \
                AND l.quantity > COALESCE((SELECT sum(fl.quantity) FROM fulfillment_lines fl \
                    JOIN fulfillments f ON f.id = fl.fulfillment_id \
                    WHERE fl.order_line_id = l.id AND f.status <> 'canceled'), 0)) THEN 'fulfilled' \
           WHEN EXISTS (SELECT 1 FROM fulfillments f WHERE f.tenant_id = $1 AND f.order_id = $2 \
                AND f.status <> 'canceled') THEN 'partially_fulfilled' \
           ELSE 'unfulfilled' END \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&order.id)
    .execute(&mut *tx)
    .await?;
    sync_payments(&mut tx, &tenant, &order.id).await?;
    let edited = load(&mut tx, &tenant, &order.id)
        .await?
        .ok_or_else(|| Error::internal("order vanished"))?;
    let collected: i64 = soat_payments::for_reference(&mut tx, &tenant, &order.id)
        .await?
        .iter()
        .filter(|p| {
            matches!(
                p.status.as_str(),
                "paid" | "partially_refunded" | "refunded"
            )
        })
        .map(|p| p.amount_minor - p.refunded_minor)
        .sum();
    tenant
        .publish(
            &mut tx,
            "order.edited",
            &json!({
                "id": edited.id, "number": edited.number, "changes": changes,
                "previous_total_minor": order.total_minor, "total_minor": edited.total_minor,
                "reason": e.reason.as_deref().unwrap_or("").trim(),
            }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(EditedOrder {
        balance_minor: edited.total_minor - collected,
        previous_total_minor: order.total_minor,
        order: edited,
    }))
}
