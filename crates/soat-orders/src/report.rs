//! What the shop sold lately: the figures behind the admin's dashboard.

use soat_tenancy::Tenant;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::ORDERS_READ;

/// The longest period a report covers, in days.
pub const MAX_DAYS: i64 = 366;

/// The report's query.
#[model]
pub struct ReportQuery {
    /// Days back from today, today included (default 30, at most 366).
    pub days: Option<i64>,
}

/// Sales in one currency. Canceled orders are left out.
#[model]
#[derive(sqlx::FromRow)]
pub struct CurrencySales {
    pub currency: String,
    pub orders: i64,
    pub total_minor: i64,
    /// The average order, rounded down.
    pub average_minor: i64,
}

/// One day's sales in one currency.
#[model]
#[derive(sqlx::FromRow)]
pub struct DaySales {
    /// `YYYY-MM-DD`, UTC.
    pub date: String,
    pub currency: String,
    pub orders: i64,
    pub total_minor: i64,
}

/// A product among the best sellers.
#[model]
#[derive(sqlx::FromRow)]
pub struct TopProduct {
    pub product_id: Option<String>,
    pub title: String,
    pub currency: String,
    pub quantity: i64,
    pub total_minor: i64,
}

/// Open orders that wait on staff, whenever they were placed.
#[model]
#[derive(sqlx::FromRow)]
pub struct ToDo {
    /// Open and not paid yet.
    pub unpaid: i64,
    /// Open and not shipped yet.
    pub unfulfilled: i64,
}

/// The sales report.
#[model]
pub struct Report {
    pub days: i64,
    /// The first day counted, `YYYY-MM-DD`, UTC.
    pub since: String,
    pub sales: Vec<CurrencySales>,
    /// Only days with orders, oldest first.
    pub by_day: Vec<DaySales>,
    /// The five that sold the most units.
    pub top_products: Vec<TopProduct>,
    pub to_do: ToDo,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((report,))
}

/// Sales over the last `?days=` days (default 30) by currency and by day,
/// the best sellers, and the open orders waiting on staff (`orders:read`).
#[get("/api/orders/report", tag = "orders")]
async fn report(db: Db, tenant: Tenant, Query(q): Query<ReportQuery>) -> Result<Json<Report>> {
    let days = q.days.unwrap_or(30);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(Error::unprocessable(format!(
            "days is between 1 and {MAX_DAYS}"
        )));
    }
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let since: String = sqlx::query_scalar(
        "SELECT to_char((now() AT TIME ZONE 'UTC')::date - ($1::bigint - 1)::int, 'YYYY-MM-DD')",
    )
    .bind(days)
    .fetch_one(&mut *tx)
    .await?;
    // Every query filters by tenant and day; `since` is a date at 00:00 UTC.
    let window = "tenant_id = $1 AND status <> 'canceled' \
         AND placed_at >= ($2::date)::timestamp AT TIME ZONE 'UTC'";
    let sales = sqlx::query_as::<_, CurrencySales>(&format!(
        "SELECT currency, count(*) AS orders, sum(total_minor)::bigint AS total_minor, \
         (sum(total_minor) / count(*))::bigint AS average_minor \
         FROM orders WHERE {window} GROUP BY currency ORDER BY sum(total_minor) DESC, currency"
    ))
    .bind(tenant.id())
    .bind(&since)
    .fetch_all(&mut *tx)
    .await?;
    let by_day = sqlx::query_as::<_, DaySales>(&format!(
        "SELECT to_char((placed_at AT TIME ZONE 'UTC')::date, 'YYYY-MM-DD') AS date, currency, \
         count(*) AS orders, sum(total_minor)::bigint AS total_minor \
         FROM orders WHERE {window} GROUP BY 1, currency ORDER BY 1, currency"
    ))
    .bind(tenant.id())
    .bind(&since)
    .fetch_all(&mut *tx)
    .await?;
    let top_products = sqlx::query_as::<_, TopProduct>(
        "SELECT l.product_id, min(l.title) AS title, o.currency, \
         sum(l.quantity)::bigint AS quantity, sum(l.total_minor)::bigint AS total_minor \
         FROM order_lines l JOIN orders o ON o.id = l.order_id AND o.tenant_id = l.tenant_id \
         WHERE l.tenant_id = $1 AND o.status <> 'canceled' \
         AND o.placed_at >= ($2::date)::timestamp AT TIME ZONE 'UTC' \
         GROUP BY l.product_id, CASE WHEN l.product_id IS NULL THEN l.sku END, o.currency \
         ORDER BY quantity DESC, total_minor DESC, title LIMIT 5",
    )
    .bind(tenant.id())
    .bind(&since)
    .fetch_all(&mut *tx)
    .await?;
    let to_do = sqlx::query_as::<_, ToDo>(
        "SELECT count(*) FILTER (WHERE financial_status = 'pending') AS unpaid, \
         count(*) FILTER (WHERE fulfillment_status = 'unfulfilled') AS unfulfilled \
         FROM orders WHERE tenant_id = $1 AND status = 'open'",
    )
    .bind(tenant.id())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(Report {
        days,
        since,
        sales,
        by_day,
        top_products,
        to_do,
    }))
}
