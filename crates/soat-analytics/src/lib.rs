//! Storefront analytics without cookies.
//!
//! The storefront marks each page it shows with a [`PageView`]; this
//! module's middleware counts the marked pages people (not bots) open. A
//! visitor is a hash of the day's salt, the shop, the address and the
//! browser: nothing is stored in the browser, no address is kept, and
//! since a day's salt is deleted the next day, visits of different days
//! cannot be linked. Views wait in memory and are written every few
//! seconds (and before a report).
//!
//! `GET /api/analytics` reports visitors, views, pages, sources and
//! devices, and the way from visit to cart to order to payment.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use soat_core::Permission;
use soat_module::Module;
use soat_tenancy::{EventShop, Scope, Tenant};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::{Request, Resources};

/// See the storefront's visitors and how they buy.
pub const ANALYTICS_READ: Permission =
    Permission::new("analytics:read", "See storefront visitors and conversion").for_staff();

/// How often waiting views are written.
pub const FLUSH_EVERY: Duration = Duration::from_secs(10);
/// Most views kept waiting; more are dropped until the next write.
const MAX_WAITING: usize = 100_000;
/// Views older than this are deleted.
const KEEP_DAYS: i32 = 400;
/// The longest period a report covers, in days.
pub const MAX_DAYS: i64 = 366;

/// Marks a response as a storefront page of shop `tenant_id`, for the
/// count. The storefront puts it in the response's extensions.
#[derive(Debug, Clone)]
pub struct PageView {
    pub tenant_id: String,
    /// The theme page, e.g. `product`.
    pub page: String,
}

/// A view waiting to be written. The address and browser stay in memory
/// only until the visitor hash is made.
#[derive(Debug, Clone)]
struct Hit {
    tenant_id: String,
    page: String,
    path: String,
    source: String,
    device: &'static str,
    who: String,
    /// Milliseconds since 1970.
    at_ms: i64,
}

/// The views waiting to be written, shared by the middleware, the
/// scheduled write and the report.
#[derive(Clone, Default)]
pub struct Tracker(Arc<Mutex<Vec<Hit>>>);

impl Tracker {
    fn push(&self, hit: Hit) {
        if let Ok(mut v) = self.0.lock() {
            if v.len() < MAX_WAITING {
                v.push(hit);
            }
        }
    }

    fn take(&self) -> Vec<Hit> {
        self.0
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }

    /// Writes the waiting views; how many.
    ///
    /// # Errors
    ///
    /// Database errors (the views are dropped then).
    pub async fn flush(&self, pool: &PgPool) -> Result<usize> {
        let hits = self.take();
        if hits.is_empty() {
            return Ok(0);
        }
        let salt = salt(pool).await?;
        let mut tenants: Vec<&str> = hits.iter().map(|h| h.tenant_id.as_str()).collect();
        tenants.sort_unstable();
        tenants.dedup();
        for tenant in tenants {
            let scope = EventShop(tenant.to_owned());
            let mut tx = scope.begin_on(pool).await?;
            for h in hits.iter().filter(|h| h.tenant_id == tenant) {
                let visitor = hash_token(&format!("{salt}|{tenant}|{}", h.who));
                sqlx::query(
                    "INSERT INTO analytics_hits (tenant_id, at, visitor, path, page, source, device) \
                     VALUES ($1, to_timestamp($2::bigint / 1000.0), $3, $4, $5, $6, $7)",
                )
                .bind(tenant)
                .bind(h.at_ms)
                .bind(&visitor[..24])
                .bind(&h.path)
                .bind(&h.page)
                .bind(&h.source)
                .bind(h.device)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(&format!(
                "DELETE FROM analytics_hits WHERE tenant_id = $1 \
                 AND at < now() - make_interval(days => {KEEP_DAYS})"
            ))
            .bind(tenant)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        Ok(hits.len())
    }
}

/// Today's salt, made when the day's first view is written; older salts
/// are deleted.
async fn salt(pool: &PgPool) -> Result<String> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM analytics_salts WHERE day < (now() AT TIME ZONE 'UTC')::date")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO analytics_salts (day, salt) VALUES ((now() AT TIME ZONE 'UTC')::date, $1) \
         ON CONFLICT DO NOTHING",
    )
    .bind(new_token()?)
    .execute(&mut *tx)
    .await?;
    let (salt,): (String,) = sqlx::query_as(
        "SELECT salt FROM analytics_salts WHERE day = (now() AT TIME ZONE 'UTC')::date",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(salt)
}

/// The analytics module.
pub struct Analytics;

impl Module for Analytics {
    fn name(&self) -> &'static str {
        "analytics"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "cart", "orders"]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[ANALYTICS_READ]
    }

    fn mount(&self, app: App) -> App {
        let tracker = Tracker::default();
        let counting = tracker.clone();
        let writing = tracker.clone();
        app.mount((report,))
            .state(tracker)
            .wrap(move |req: Request, next: Next| {
                let tracker = counting.clone();
                async move { track(&tracker, req, next).await }
            })
            .schedule(
                zapline::tasks::Schedule::every(FLUSH_EVERY).name("soat-analytics-flush"),
                move |res: Resources| {
                    let tracker = writing.clone();
                    async move {
                        let pool: PgPool = res.require()?;
                        tracker.flush(&pool).await.map(|_| ())
                    }
                },
            )
    }
}

/// What a request says about who opens the page and from where; None for
/// requests that are not counted.
struct Seen {
    path: String,
    source: String,
    device: &'static str,
    who: String,
}

const BOTS: [&str; 12] = [
    "bot",
    "crawl",
    "spider",
    "slurp",
    "preview",
    "headless",
    "curl",
    "wget",
    "python",
    "http",
    "lighthouse",
    "facebookexternalhit",
];

fn seen(req: &Request) -> Option<Seen> {
    if req.method() != Method::GET {
        return None;
    }
    let path = req.path();
    if ["/api/", "/admin", "/assets/", "/docs", "/files/", "/media/"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        return None;
    }
    let h = req.headers();
    let text = |name: &str| h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("");
    let agent = text("user-agent");
    let lower = agent.to_ascii_lowercase();
    if agent.is_empty() || BOTS.iter().any(|b| lower.contains(b)) {
        return None;
    }
    if text("sec-purpose").contains("prefetch") || text("purpose").contains("prefetch") {
        return None;
    }
    let address = match text("x-forwarded-for").split(',').next().map(str::trim) {
        Some(a) if !a.is_empty() => a.to_owned(),
        _ => req
            .conn()
            .remote_addr
            .map(|a| a.ip().to_string())
            .unwrap_or_default(),
    };
    let device = if lower.contains("ipad") || lower.contains("tablet") {
        "tablet"
    } else if lower.contains("mobi") || lower.contains("android") {
        "mobile"
    } else {
        "desktop"
    };
    Some(Seen {
        path: path.chars().take(300).collect(),
        source: source(req.uri().query(), text("referer"), text("host")),
        device,
        who: format!("{address}|{agent}"),
    })
}

/// Where a visit came from: `utm_source`, else the referring site unless
/// it is the shop itself; empty for direct visits.
fn source(query: Option<&str>, referer: &str, host: &str) -> String {
    let clean = |s: &str| -> String {
        let s = s.trim().to_ascii_lowercase();
        let s = s.strip_prefix("www.").unwrap_or(&s);
        s.chars().take(100).collect()
    };
    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("utm_source=") {
                let v = v.replace('+', " ");
                if !v.is_empty() {
                    return clean(&v);
                }
            }
        }
    }
    let site = referer
        .split("://")
        .nth(1)
        .and_then(|r| r.split(['/', '?', '#']).next())
        .unwrap_or("");
    let site = site.rsplit('@').next().unwrap_or(site);
    if site.is_empty() || clean(site) == clean(host) {
        return String::new();
    }
    clean(site.split(':').next().unwrap_or(site))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

async fn track(tracker: &Tracker, req: Request, next: Next) -> Response {
    let Some(seen) = seen(&req) else {
        return next.run(req).await;
    };
    let res = next.run(req).await;
    if res.status() == StatusCode::OK {
        if let Some(view) = res.extensions().get::<PageView>() {
            tracker.push(Hit {
                tenant_id: view.tenant_id.clone(),
                page: view.page.clone(),
                path: seen.path,
                source: seen.source,
                device: seen.device,
                who: seen.who,
                at_ms: now_ms(),
            });
        }
    }
    res
}

/// The report's query.
#[model]
pub struct ReportQuery {
    /// Days back from today, today included (default 30, at most 366).
    pub days: Option<i64>,
}

/// One day's traffic.
#[model]
#[derive(sqlx::FromRow)]
pub struct DayTraffic {
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    pub visitors: i64,
    pub views: i64,
}

/// A page and how many saw it.
#[model]
#[derive(sqlx::FromRow)]
pub struct PageTraffic {
    pub path: String,
    pub views: i64,
    pub visitors: i64,
}

/// Visitors by one value of something: a source or a device.
#[model]
#[derive(sqlx::FromRow)]
pub struct Share {
    /// A source (`direct` for none) or a device (`desktop`, `mobile`,
    /// `tablet`).
    pub name: String,
    pub visitors: i64,
}

/// From visit to paid order.
#[model]
pub struct Funnel {
    pub visitors: i64,
    /// Carts started that got at least one line.
    pub carts: i64,
    /// Orders placed, canceled ones left out.
    pub orders: i64,
    /// Those orders that were paid.
    pub paid: i64,
    /// Orders per visitor, in hundredths of a percent (250 is 2.5%).
    pub conversion_bps: i64,
}

/// Orders by buyers' first and later orders.
#[model]
pub struct Buyers {
    /// Orders by a buyer (an email) who never ordered before.
    pub first_orders: i64,
    /// Orders by a buyer who ordered before.
    pub repeat_orders: i64,
}

/// The analytics report.
#[model]
pub struct Report {
    /// The first day counted, `YYYY-MM-DD`, UTC.
    pub from: String,
    pub visitors: i64,
    pub views: i64,
    /// Only days with views, oldest first.
    pub days: Vec<DayTraffic>,
    /// The ten most viewed pages.
    pub pages: Vec<PageTraffic>,
    /// The ten sources that brought the most visitors.
    pub sources: Vec<Share>,
    pub devices: Vec<Share>,
    pub funnel: Funnel,
    pub buyers: Buyers,
}

/// The report of shop `scope` from day `from` (`YYYY-MM-DD`, UTC).
///
/// # Errors
///
/// Database errors.
pub async fn build(conn: &mut PgConnection, scope: &impl Scope, from: &str) -> Result<Report> {
    const SINCE: &str = "at >= ($2::date)::timestamp AT TIME ZONE 'UTC'";
    let t = scope.tenant_id();
    let (visitors, views): (i64, i64) = sqlx::query_as(&format!(
        "SELECT COUNT(DISTINCT visitor), COUNT(*) FROM analytics_hits WHERE tenant_id = $1 AND {SINCE}"
    ))
    .bind(t)
    .bind(from)
    .fetch_one(&mut *conn)
    .await?;
    let days: Vec<DayTraffic> = sqlx::query_as(&format!(
        "SELECT to_char((at AT TIME ZONE 'UTC')::date, 'YYYY-MM-DD') AS day, \
         COUNT(DISTINCT visitor) AS visitors, COUNT(*) AS views FROM analytics_hits \
         WHERE tenant_id = $1 AND {SINCE} GROUP BY 1 ORDER BY 1"
    ))
    .bind(t)
    .bind(from)
    .fetch_all(&mut *conn)
    .await?;
    let pages: Vec<PageTraffic> = sqlx::query_as(&format!(
        "SELECT path, COUNT(*) AS views, COUNT(DISTINCT visitor) AS visitors FROM analytics_hits \
         WHERE tenant_id = $1 AND {SINCE} GROUP BY path ORDER BY views DESC, path LIMIT 10"
    ))
    .bind(t)
    .bind(from)
    .fetch_all(&mut *conn)
    .await?;
    let sources: Vec<Share> = sqlx::query_as(&format!(
        "SELECT CASE WHEN source = '' THEN 'direct' ELSE source END AS name, \
         COUNT(DISTINCT visitor) AS visitors FROM analytics_hits \
         WHERE tenant_id = $1 AND {SINCE} GROUP BY 1 ORDER BY visitors DESC, name LIMIT 10"
    ))
    .bind(t)
    .bind(from)
    .fetch_all(&mut *conn)
    .await?;
    let devices: Vec<Share> = sqlx::query_as(&format!(
        "SELECT device AS name, COUNT(DISTINCT visitor) AS visitors FROM analytics_hits \
         WHERE tenant_id = $1 AND {SINCE} GROUP BY 1 ORDER BY visitors DESC, name"
    ))
    .bind(t)
    .bind(from)
    .fetch_all(&mut *conn)
    .await?;
    let (carts,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM carts c WHERE c.tenant_id = $1 \
         AND c.created_at >= ($2::date)::timestamp AT TIME ZONE 'UTC' \
         AND (c.status = 'completed' OR EXISTS (SELECT 1 FROM cart_lines l WHERE l.cart_id = c.id))",
    )
    .bind(t)
    .bind(from)
    .fetch_one(&mut *conn)
    .await?;
    let (orders, paid, first_orders): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), \
         COUNT(*) FILTER (WHERE o.financial_status IN ('paid', 'partially_refunded', 'refunded')), \
         COUNT(*) FILTER (WHERE NOT EXISTS (SELECT 1 FROM orders p WHERE p.tenant_id = o.tenant_id \
           AND lower(p.email) = lower(o.email) AND p.status <> 'canceled' \
           AND (p.placed_at, p.id) < (o.placed_at, o.id))) \
         FROM orders o WHERE o.tenant_id = $1 AND o.status <> 'canceled' \
         AND o.placed_at >= ($2::date)::timestamp AT TIME ZONE 'UTC'",
    )
    .bind(t)
    .bind(from)
    .fetch_one(&mut *conn)
    .await?;
    let conversion_bps = if visitors > 0 {
        orders.saturating_mul(10_000) / visitors
    } else {
        0
    };
    Ok(Report {
        from: from.to_owned(),
        visitors,
        views,
        days,
        pages,
        sources,
        devices,
        funnel: Funnel {
            visitors,
            carts,
            orders,
            paid,
            conversion_bps,
        },
        buyers: Buyers {
            first_orders,
            repeat_orders: orders - first_orders,
        },
    })
}

/// Storefront visitors, views, pages, sources and devices over the last
/// `?days=` days (default 30), and how many of them went on to a cart,
/// an order and a payment (`analytics:read`). Counted without cookies:
/// one visitor is one browser on one day.
#[get("/api/analytics", tag = "analytics")]
async fn report(
    db: Db,
    tenant: Tenant,
    State(tracker): State<Tracker>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Report>> {
    let days = q.days.unwrap_or(30);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(
            Error::unprocessable(format!("days is 1 to {MAX_DAYS}")).with_code("validation_error")
        );
    }
    let mut tx = tenant.begin_for(&db, &ANALYTICS_READ).await?;
    tracker.flush(&db.0).await?;
    let (from,): (String,) =
        sqlx::query_as("SELECT to_char((now() AT TIME ZONE 'UTC')::date - $1::int, 'YYYY-MM-DD')")
            .bind(i32::try_from(days - 1).unwrap_or(0))
            .fetch_one(&mut *tx)
            .await?;
    let r = build(&mut tx, &tenant, &from).await?;
    tx.commit().await?;
    Ok(Json(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_name_where_visits_come_from() {
        assert_eq!(
            source(Some("utm_source=Newsletter&x=1"), "", "shop.example"),
            "newsletter"
        );
        assert_eq!(
            source(None, "https://www.Google.com/search?q=mug", "shop.example"),
            "google.com"
        );
        assert_eq!(
            source(None, "https://shop.example/cart", "shop.example"),
            ""
        );
        assert_eq!(
            source(None, "http://localhost:8080/", "shop.example"),
            "localhost"
        );
        assert_eq!(source(None, "", "shop.example"), "");
        assert_eq!(source(Some("utm_source="), "", "shop.example"), "");
    }
}
