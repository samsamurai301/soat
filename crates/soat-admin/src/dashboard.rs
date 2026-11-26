//! A shop's home: what needs doing, how sales go, and where to start.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::money;
use crate::{Ctx, Shared};

/// The periods the dashboard offers, in days.
const PERIODS: [i64; 3] = [7, 30, 90];

/// The chart's height in SVG units; bars are as tall as their share of the
/// best day.
const CHART_HEIGHT: i64 = 120;

/// The dashboard's query.
#[model]
pub struct HomeQuery {
    pub days: Option<i64>,
}

/// A shop's home: orders waiting on staff, sales over the last days, the
/// best sellers, and first steps for a new shop.
#[get("/admin/{shop}", tag = "admin")]
pub(crate) async fn home(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(q): Query<HomeQuery>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    let days = q.days.filter(|d| PERIODS.contains(d)).unwrap_or(30);
    let (status, products) = cx.call(Method::GET, "/api/products?limit=1", None).await;
    let product_count = if status == 200 {
        products["total"].as_i64()
    } else {
        None
    };
    cx.set("product_count", json!(product_count));
    let unfinished = crate::onboarding::unfinished(&cx).await;
    cx.set("first_steps", json!(unfinished));
    // Staff without `orders:read` see the rest of the page without sales.
    let (status, report) = cx
        .call(
            Method::GET,
            &format!("/api/orders/report?days={days}"),
            None,
        )
        .await;
    if status == 200 {
        cx.set("report", view(&report, days));
    }
    cx.set(
        "periods",
        json!(PERIODS
            .iter()
            .map(|p| json!({ "days": p, "current": *p == days }))
            .collect::<Vec<_>>()),
    );
    let title = cx.data["shop"]["name"].clone();
    cx.set("page_title", title);
    Ok(cx.render("home", StatusCode::OK))
}

/// The report as the page shows it: amounts formatted, and a bar per day in
/// the currency that sold the most.
fn view(r: &Value, days: i64) -> Value {
    let list = |k: &str| r[k].as_array().cloned().unwrap_or_default();
    let int = |v: &Value| v.as_i64().unwrap_or(0);
    let sales: Vec<Value> = list("sales")
        .iter()
        .map(|s| {
            let c = s["currency"].as_str().unwrap_or_default();
            json!({
                "currency": c, "orders": s["orders"],
                "total": money(int(&s["total_minor"]), c),
                "average": money(int(&s["average_minor"]), c),
            })
        })
        .collect();
    let top: Vec<Value> = list("top_products")
        .iter()
        .map(|p| {
            let c = p["currency"].as_str().unwrap_or_default();
            json!({
                "title": p["title"], "product_id": p["product_id"], "quantity": p["quantity"],
                "total": money(int(&p["total_minor"]), c),
            })
        })
        .collect();
    let main = list("sales")
        .first()
        .and_then(|s| s["currency"].as_str().map(str::to_owned));
    let chart = main.map(|currency| {
        let start = r["since"].as_str().and_then(day_number).unwrap_or(0);
        let mut totals = vec![0i64; usize::try_from(days).unwrap_or(0)];
        for d in list("by_day") {
            if d["currency"].as_str() != Some(currency.as_str()) {
                continue;
            }
            let at = d["date"].as_str().and_then(day_number).map(|n| n - start);
            if let Some(slot) = at
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| totals.get_mut(i))
            {
                *slot += int(&d["total_minor"]);
            }
        }
        let best = totals.iter().copied().max().unwrap_or(0).max(1);
        let bars: Vec<Value> = totals
            .iter()
            .enumerate()
            .map(|(i, &t)| {
                let h = (t.saturating_mul(CHART_HEIGHT) / best).max(i64::from(t > 0));
                let date = date_of(start + i64::try_from(i).unwrap_or(0));
                json!({
                    "x": i * 10, "y": CHART_HEIGHT - h, "height": h,
                    "label": format!("{date}: {}", money(t, &currency)),
                })
            })
            .collect();
        json!({
            "currency": currency, "bars": bars, "width": totals.len() * 10,
            "height": CHART_HEIGHT, "best": money(best, &currency),
        })
    });
    json!({
        "days": days, "since": r["since"], "sales": sales, "top": top, "chart": chart,
        "has_sales": !list("sales").is_empty(),
        "unpaid": r["to_do"]["unpaid"], "unfulfilled": r["to_do"]["unfulfilled"],
    })
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date.
pub(crate) fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-').map(str::parse::<i64>);
    let (y, m, d) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The `YYYY-MM-DD` date of a day number.
pub(crate) fn date_of(n: i64) -> String {
    let z = n + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_round_trip() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("2026-11-23"), Some(20_780));
        assert_eq!(date_of(20_780), "2026-11-23");
        for n in [-1, 59, 60, 11_016, 20_000, 30_000] {
            assert_eq!(day_number(&date_of(n)), Some(n));
        }
        assert_eq!(date_of(day_number("2028-02-29").unwrap() + 1), "2028-03-01");
        assert_eq!(day_number("2026-13-01"), None);
        assert_eq!(day_number("soon"), None);
    }

    #[test]
    fn the_chart_fills_quiet_days() {
        let r = json!({
            "since": "2026-11-21",
            "sales": [{ "currency": "EUR", "orders": 2, "total_minor": 3000, "average_minor": 1500 }],
            "by_day": [
                { "date": "2026-11-21", "currency": "EUR", "orders": 1, "total_minor": 1000 },
                { "date": "2026-11-23", "currency": "EUR", "orders": 1, "total_minor": 2000 },
                { "date": "2026-11-23", "currency": "CHF", "orders": 1, "total_minor": 9000 }
            ],
            "top_products": [], "to_do": { "unpaid": 1, "unfulfilled": 0 }
        });
        let v = view(&r, 3);
        let bars = v["chart"]["bars"].as_array().unwrap();
        let heights: Vec<i64> = bars.iter().map(|b| b["height"].as_i64().unwrap()).collect();
        assert_eq!(heights, [60, 0, 120]);
        assert_eq!(bars[1]["label"], "2026-11-22: 0.00 EUR");
        assert_eq!(v["sales"][0]["average"], "15.00 EUR");
    }
}
