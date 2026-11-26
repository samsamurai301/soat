//! Storefront analytics: visitors per day, the pages they open, where
//! they come from, and how many go on to buy.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::dashboard::{date_of, day_number};
use crate::{Ctx, Shared};

/// The periods the page offers, in days.
const PERIODS: [i64; 4] = [7, 30, 90, 365];
/// The chart's height in SVG units.
const CHART_HEIGHT: i64 = 120;

/// The analytics page's query.
#[model]
pub struct AnalyticsQuery {
    pub days: Option<i64>,
}

/// A percentage with one decimal from hundredths of a percent.
fn percent(bps: i64) -> String {
    format!("{}.{}%", bps / 100, (bps % 100) / 10)
}

/// Visitors and conversion over the last days.
#[get("/admin/{shop}/analytics", tag = "admin")]
pub(crate) async fn page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(q): Query<AnalyticsQuery>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "analytics").await);
    let days = q.days.filter(|d| PERIODS.contains(d)).unwrap_or(30);
    cx.set("page_title", json!("Analytics"));
    cx.set(
        "periods",
        json!(PERIODS
            .iter()
            .map(|p| json!({ "days": p, "current": *p == days }))
            .collect::<Vec<_>>()),
    );
    let (status, r) = cx
        .call(Method::GET, &format!("/api/analytics?days={days}"), None)
        .await;
    match status {
        200 => cx.set("report", view(&r, days)),
        404 => cx.set("missing", json!(true)),
        401 => return Ok(crate::to_login()),
        _ => return Ok(cx.failed(status, &r)),
    }
    Ok(cx.render("analytics", StatusCode::OK))
}

fn view(r: &Value, days: i64) -> Value {
    let int = |v: &Value| v.as_i64().unwrap_or(0);
    let start = r["from"].as_str().and_then(day_number).unwrap_or(0);
    let mut visitors = vec![0i64; usize::try_from(days).unwrap_or(0)];
    for d in r["days"].as_array().into_iter().flatten() {
        let at = d["day"].as_str().and_then(day_number).map(|n| n - start);
        if let Some(slot) = at
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| visitors.get_mut(i))
        {
            *slot = int(&d["visitors"]);
        }
    }
    let best = visitors.iter().copied().max().unwrap_or(0).max(1);
    let bars: Vec<Value> = visitors
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let h = (v.saturating_mul(CHART_HEIGHT) / best).max(i64::from(v > 0));
            let date = date_of(start + i64::try_from(i).unwrap_or(0));
            json!({ "x": i * 10, "y": CHART_HEIGHT - h, "height": h,
                    "label": format!("{date}: {v} visitors") })
        })
        .collect();
    let f = &r["funnel"];
    let step = |n: &Value| {
        let n = int(n);
        let of = int(&f["visitors"]);
        json!({ "count": n, "share": if of > 0 { percent(n.saturating_mul(10_000) / of) } else { "0.0%".into() } })
    };
    json!({
        "from": r["from"], "visitors": r["visitors"], "views": r["views"],
        "has_views": int(&r["views"]) > 0,
        "chart": { "bars": bars, "width": visitors.len() * 10, "height": CHART_HEIGHT, "best": best },
        "pages": r["pages"], "sources": r["sources"], "devices": r["devices"],
        "funnel": { "visitors": f["visitors"], "carts": step(&f["carts"]), "orders": step(&f["orders"]),
                    "paid": step(&f["paid"]), "conversion": percent(int(&f["conversion_bps"])) },
        "buyers": r["buyers"],
    })
}
