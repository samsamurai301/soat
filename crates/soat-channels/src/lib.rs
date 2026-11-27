//! Sales channels: product feeds that Google Merchant Center and Meta
//! catalogs fetch on a schedule.
//!
//! Staff make a feed (`/api/feeds`) in a format (`google`, RSS 2.0 with
//! the `g:` namespace, or `meta`, CSV) and a currency, optionally for one
//! collection. Its URL (`/feeds/{token}`) lists one item per active
//! product without variants and one per variant (grouped by product),
//! with the price buyers pay in that currency (a price list's
//! compare-at price becomes the regular price and the lower one the sale
//! price), stock, images, brand, GTIN and weight. Brand, GTIN and MPN come
//! from the product's custom fields `brand`, `gtin` and `mpn` when it has
//! them. The preview (`/api/feeds/{id}/preview`) names what the channel
//! would reject, such as an item without a picture.

use serde_json::{json, Value};
use soat_core::feeds::{self, Availability, FeedItem};
use soat_core::{id_kind, Currency, Permission};
use soat_module::Module;
use soat_tenancy::{Scope, Shop, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::storage::Storage;

id_kind!(FeedId, FeedKind, "fed");

/// See the shop's product feeds and their problems.
pub const CHANNELS_READ: Permission =
    Permission::new("channels:read", "See product feeds and their problems").for_staff();
/// Make, change and delete product feeds.
pub const CHANNELS_MANAGE: Permission =
    Permission::new("channels:manage", "Make, change and delete product feeds").for_staff();

/// Most items in one feed.
const MAX_ITEMS: i64 = 20_000;

/// The channels module.
pub struct Channels;

impl Module for Channels {
    fn name(&self) -> &'static str {
        "channels"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "files",
            "catalog",
            "inventory",
            "pricing",
        ]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[CHANNELS_READ, CHANNELS_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_feeds,
            create_feed,
            update_feed,
            delete_feed,
            new_token,
            preview,
            serve,
        ))
    }
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_feed")
}

/// A product feed.
#[model]
#[derive(sqlx::FromRow)]
pub struct Feed {
    /// e.g. `fed_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub name: String,
    /// `google` or `meta`.
    pub format: String,
    pub currency: String,
    /// The URL's path; give the channel the shop's address with it.
    pub path: String,
    pub brand: String,
    pub collection_handle: Option<String>,
    pub include_out_of_stock: bool,
    pub active: bool,
    /// When a channel last fetched it.
    pub fetched_at: Option<String>,
    pub created_at: String,
}

const COLUMNS: &str = "id, name, format, currency, '/feeds/' || token AS path, brand, \
     collection_handle, include_out_of_stock, active, \
     to_char(fetched_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS fetched_at, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

#[model]
pub struct FeedList {
    pub items: Vec<Feed>,
}

/// A new feed.
#[model]
pub struct NewFeed {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    /// `google` or `meta`.
    pub format: String,
    /// Defaults to the shop's main currency.
    pub currency: Option<String>,
    #[validate(length(max = 100))]
    pub brand: Option<String>,
    pub collection_handle: Option<String>,
    pub include_out_of_stock: Option<bool>,
}

/// Changes to a feed; what is left out stays. An empty
/// `collection_handle` lists every product again.
#[model]
pub struct FeedChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    pub currency: Option<String>,
    #[validate(length(max = 100))]
    pub brand: Option<String>,
    pub collection_handle: Option<String>,
    pub include_out_of_stock: Option<bool>,
    pub active: Option<bool>,
}

/// What a feed's event carries: never its token.
fn event(f: &Feed) -> Value {
    json!({ "id": f.id, "name": f.name, "format": f.format, "currency": f.currency,
            "active": f.active })
}

fn token() -> Result<String> {
    Ok(zapline::auth::password::new_token()?
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(32)
        .collect())
}

async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Feed> {
    let id = FeedId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM product_feeds WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

fn currency(raw: &str) -> Result<String> {
    Ok(Currency::parse(raw.trim())
        .map_err(|e| invalid(e.to_string()))?
        .as_str()
        .to_owned())
}

fn collection(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_owned)
}

/// The shop's product feeds (`channels:read`).
#[get("/api/feeds", tag = "channels")]
async fn list_feeds(db: Db, tenant: Tenant) -> Result<Json<FeedList>> {
    let mut tx = tenant.begin_for(&db, &CHANNELS_READ).await?;
    let items = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM product_feeds WHERE tenant_id = $1 ORDER BY id LIMIT 100"
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(FeedList { items }))
}

/// Make a product feed (`channels:manage`). Publishes `feed.created`.
#[post("/api/feeds", tag = "channels", status = 201)]
async fn create_feed(
    db: Db,
    tenant: Tenant,
    Valid(Json(n)): Valid<Json<NewFeed>>,
) -> Result<(StatusCode, Json<Feed>)> {
    if !["google", "meta"].contains(&n.format.as_str()) {
        return Err(invalid("format is google or meta"));
    }
    let mut tx = tenant.begin_for(&db, &CHANNELS_MANAGE).await?;
    let cur = match n.currency.as_deref() {
        Some(c) => currency(c)?,
        None => soat_catalog::storefront::main_currency(&mut tx, &tenant)
            .await?
            .unwrap_or_else(|| "EUR".to_owned()),
    };
    let id = FeedId::new();
    sqlx::query(
        "INSERT INTO product_feeds (id, tenant_id, name, format, currency, token, brand, \
         collection_handle, include_out_of_stock) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(n.name.trim())
    .bind(&n.format)
    .bind(&cur)
    .bind(token()?)
    .bind(n.brand.as_deref().unwrap_or("").trim())
    .bind(collection(n.collection_handle.as_deref()))
    .bind(n.include_out_of_stock.unwrap_or(true))
    .execute(&mut *tx)
    .await?;
    let f = load(&mut tx, &tenant, id.as_str()).await?;
    tenant.publish(&mut tx, "feed.created", &event(&f)).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(f)))
}

/// Change a feed (`channels:manage`). Publishes `feed.updated`.
#[patch("/api/feeds/{id}", tag = "channels")]
async fn update_feed(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<FeedChange>>,
) -> Result<Json<Feed>> {
    let mut tx = tenant.begin_for(&db, &CHANNELS_MANAGE).await?;
    let before = load(&mut tx, &tenant, &id).await?;
    let cur = c.currency.as_deref().map(currency).transpose()?;
    let handle = match c.collection_handle.as_deref() {
        Some(h) => collection(Some(h)),
        None => before.collection_handle.clone(),
    };
    sqlx::query(
        "UPDATE product_feeds SET name = COALESCE($3, name), currency = COALESCE($4, currency), \
         brand = COALESCE($5, brand), collection_handle = $6, \
         include_out_of_stock = COALESCE($7, include_out_of_stock), \
         active = COALESCE($8, active) WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&before.id)
    .bind(c.name.as_deref().map(str::trim))
    .bind(cur)
    .bind(c.brand.as_deref().map(str::trim))
    .bind(handle)
    .bind(c.include_out_of_stock)
    .bind(c.active)
    .execute(&mut *tx)
    .await?;
    let f = load(&mut tx, &tenant, &before.id).await?;
    tenant.publish(&mut tx, "feed.updated", &event(&f)).await?;
    tx.commit().await?;
    Ok(Json(f))
}

/// Delete a feed (`channels:manage`); its URL stops working. Publishes
/// `feed.deleted`.
#[delete("/api/feeds/{id}", tag = "channels", status = 204)]
async fn delete_feed(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CHANNELS_MANAGE).await?;
    let f = load(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM product_feeds WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&f.id)
        .execute(&mut *tx)
        .await?;
    tenant.publish(&mut tx, "feed.deleted", &event(&f)).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Give a feed a new URL, when the old one got out (`channels:manage`);
/// the old one stops working. Publishes `feed.updated`.
#[post("/api/feeds/{id}/token", tag = "channels")]
async fn new_token(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Feed>> {
    let mut tx = tenant.begin_for(&db, &CHANNELS_MANAGE).await?;
    let f = load(&mut tx, &tenant, &id).await?;
    sqlx::query("UPDATE product_feeds SET token = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&f.id)
        .bind(token()?)
        .execute(&mut *tx)
        .await?;
    let f = load(&mut tx, &tenant, &f.id).await?;
    tenant.publish(&mut tx, "feed.updated", &event(&f)).await?;
    tx.commit().await?;
    Ok(Json(f))
}

/// The shop's address, as the storefront uses it: the domain it was
/// reached on when it is one of the shop's, else its first domain, else
/// the request's host.
async fn origin(
    conn: &mut PgConnection,
    scope: &impl Scope,
    headers: &HeaderMap,
) -> Result<String> {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_ascii_lowercase)
        .filter(|h| {
            !h.is_empty()
                && h.len() <= 255
                && h.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')
                })
        });
    let domains: Vec<(String,)> = sqlx::query_as(
        "SELECT domain FROM tenant_domains WHERE tenant_id = $1 ORDER BY created_at, domain",
    )
    .bind(scope.tenant_id())
    .fetch_all(conn)
    .await?;
    let bare = |h: &str| h.split(':').next().unwrap_or_default().to_owned();
    let chosen = match host {
        Some(h) if domains.iter().any(|(d,)| *d == bare(&h)) => Some(h),
        _ => domains.into_iter().next().map(|(d,)| d).or(host),
    };
    Ok(chosen.map_or_else(String::new, |h| {
        let local = h.starts_with("localhost") || h.starts_with("127.") || h.starts_with('[');
        format!("{}://{h}", if local { "http" } else { "https" })
    }))
}

#[derive(sqlx::FromRow)]
struct ProductRow {
    id: String,
    sku: String,
    title: String,
    handle: String,
    description: String,
    seo_description: String,
    weight_grams: Option<i64>,
}

/// Feed `f`'s items, for a shop at `base`.
///
/// # Errors
///
/// Database errors.
pub async fn items(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    base: &str,
    f: &Feed,
) -> Result<Vec<FeedItem>> {
    let cur = Currency::parse(&f.currency).map_err(|e| invalid(e.to_string()))?;
    let products: Vec<ProductRow> = sqlx::query_as(
        "SELECT p.id, p.sku, p.title, p.handle, p.description, p.seo_description, p.weight_grams \
         FROM products p WHERE p.tenant_id = $1 AND p.status = 'active' \
         AND ($2::text IS NULL OR p.id IN (SELECT cp.product_id FROM collection_products cp \
              JOIN collections c ON c.id = cp.collection_id \
              WHERE c.tenant_id = $1 AND c.handle = $2)) \
         ORDER BY p.id LIMIT $3",
    )
    .bind(scope.tenant_id())
    .bind(&f.collection_handle)
    .bind(MAX_ITEMS)
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<String> = products.iter().map(|p| p.id.clone()).collect();
    let variants: Vec<(String, String, String, Vec<String>)> = sqlx::query_as(
        "SELECT id, product_id, sku, option_values FROM product_variants \
         WHERE tenant_id = $1 AND product_id = ANY($2) ORDER BY product_id, id",
    )
    .bind(scope.tenant_id())
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let media: Vec<(String, String)> = sqlx::query_as(
        "SELECT product_id, file_id FROM product_media WHERE tenant_id = $1 \
         AND product_id = ANY($2) ORDER BY product_id, position",
    )
    .bind(scope.tenant_id())
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let files: Vec<String> = media.iter().map(|(_, f)| f.clone()).collect();
    let views = soat_files::views(conn, scope, storage, &files).await?;
    let absolute = |u: &str| {
        if u.starts_with('/') {
            format!("{base}{u}")
        } else {
            u.to_owned()
        }
    };
    let fields: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT resource_id, key, value #>> '{}' FROM field_values WHERE tenant_id = $1 \
         AND resource = 'product' AND resource_id = ANY($2) AND key IN ('brand', 'gtin', 'mpn') \
         AND jsonb_typeof(value) IN ('string', 'number')",
    )
    .bind(scope.tenant_id())
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let field = |product: &str, key: &str| {
        fields
            .iter()
            .find(|(p, k, _)| p == product && k == key)
            .map(|(_, _, v)| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let shop_brand = if f.brand.is_empty() {
        let (name,): (String,) = sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
            .bind(scope.tenant_id())
            .fetch_one(&mut *conn)
            .await?;
        name
    } else {
        f.brand.clone()
    };
    let mut out = Vec::new();
    for p in &products {
        let images: Vec<String> = media
            .iter()
            .filter(|(id, _)| *id == p.id)
            .filter_map(|(_, file)| views.iter().find(|v| &v.id == file))
            .map(|v| absolute(&v.url))
            .collect();
        let description = {
            let d = soat_core::markdown::to_text(&p.description, 5000);
            if d.trim().is_empty() {
                p.seo_description.clone()
            } else {
                d
            }
        };
        let own: Vec<_> = variants.iter().filter(|v| v.1 == p.id).collect();
        let entries: Vec<(String, Option<String>, String, String)> = if own.is_empty() {
            vec![(p.id.clone(), None, p.title.clone(), p.sku.clone())]
        } else {
            own.iter()
                .map(|(id, _, sku, values)| {
                    (
                        id.clone(),
                        Some(p.id.clone()),
                        format!("{} - {}", p.title, values.join(" / ")),
                        sku.clone(),
                    )
                })
                .collect()
        };
        for (item, group, title, sku) in entries {
            let Some(price) = soat_pricing::resolve(conn, scope, &item, cur).await? else {
                continue;
            };
            let a = soat_inventory::reservations::availability(conn, scope, &item).await?;
            let availability = if !a.tracked || a.available() > 0 {
                Availability::InStock
            } else if a.allow_backorder {
                Availability::Backorder
            } else {
                Availability::OutOfStock
            };
            if availability == Availability::OutOfStock && !f.include_out_of_stock {
                continue;
            }
            let (regular, sale) = match price.compare_at_minor {
                Some(c) if c > price.price_minor => (c, Some(price.price_minor)),
                _ => (price.price_minor, None),
            };
            out.push(FeedItem {
                id: item.clone(),
                item_group_id: group,
                title,
                description: description.clone(),
                link: format!("{base}/products/{}", p.handle),
                image_link: images.first().cloned(),
                additional_images: images.iter().skip(1).cloned().collect(),
                availability,
                price_minor: regular,
                sale_price_minor: sale,
                currency: cur,
                brand: field(&p.id, "brand").unwrap_or_else(|| shop_brand.clone()),
                gtin: field(&p.id, "gtin"),
                mpn: field(&p.id, "mpn").unwrap_or(sku),
                weight_grams: p.weight_grams,
                product_type: None,
            });
        }
    }
    Ok(out)
}

fn item_json(i: &FeedItem) -> Value {
    json!({
        "id": i.id, "item_group_id": i.item_group_id, "title": i.title,
        "link": i.link, "image_link": i.image_link,
        "availability": i.availability.google(),
        "price": i.currency.format(i.price_minor),
        "sale_price": i.sale_price_minor.map(|s| i.currency.format(s)),
        "brand": i.brand, "gtin": i.gtin, "mpn": i.mpn,
    })
}

/// What a channel will see in a feed: its items and their problems.
#[model]
pub struct Preview {
    pub count: usize,
    /// Problems the channel would reject an item for.
    pub errors: usize,
    /// Problems that make an item show worse.
    pub warnings: usize,
    /// Each problem: `item_id`, `title`, `severity` and `code` (e.g.
    /// `missing_image`, `missing_gtin`); at most 500.
    pub problems: Vec<Value>,
    /// The first 50 items.
    pub items: Vec<Value>,
}

/// A feed's items as a channel sees them, and what it would reject
/// (`channels:read`).
#[get("/api/feeds/{id}/preview", tag = "channels")]
async fn preview(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Preview>> {
    let mut tx = tenant.begin_for(&db, &CHANNELS_READ).await?;
    let f = load(&mut tx, &tenant, &id).await?;
    let base = origin(&mut tx, &tenant, &headers).await?;
    let list = items(&mut tx, &tenant, &storage, &base, &f).await?;
    tx.commit().await?;
    let found: Vec<(feeds::Problem, String)> = list
        .iter()
        .flat_map(|i| feeds::problems(i).into_iter().map(|p| (p, i.title.clone())))
        .collect();
    let errors = found.iter().filter(|(p, _)| p.severity == "error").count();
    Ok(Json(Preview {
        count: list.len(),
        errors,
        warnings: found.len() - errors,
        problems: found
            .iter()
            .take(500)
            .map(|(p, title)| {
                json!({ "item_id": p.item_id, "title": title, "severity": p.severity,
                        "code": p.code })
            })
            .collect(),
        items: list.iter().take(50).map(item_json).collect(),
    }))
}

/// A product feed, for the channel that fetches it: Google's RSS or
/// Meta's CSV. An unknown or turned-off feed is a 404.
#[get("/feeds/{token}", tag = "channels")]
async fn serve(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Response> {
    let token = token
        .trim_end_matches(".xml")
        .trim_end_matches(".csv")
        .to_owned();
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(Error::not_found());
    }
    let mut tx = shop.begin(&db).await?;
    let found: Option<Feed> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM product_feeds WHERE tenant_id = $1 AND token = $2 AND active"
    ))
    .bind(shop.id())
    .bind(&token)
    .fetch_optional(&mut *tx)
    .await?;
    let f = found.ok_or_else(Error::not_found)?;
    let base = origin(&mut tx, &shop, &headers).await?;
    let list = items(&mut tx, &shop, &storage, &base, &f).await?;
    sqlx::query("UPDATE product_feeds SET fetched_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(shop.id())
        .bind(&f.id)
        .execute(&mut *tx)
        .await?;
    let (name,): (String,) = sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
        .bind(shop.id())
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    let (body, kind) = if f.format == "meta" {
        (feeds::meta_csv(&list), "text/csv; charset=utf-8")
    } else {
        (
            feeds::google_rss(&name, &base, &list),
            "application/xml; charset=utf-8",
        )
    };
    let mut res = body.into_response();
    if let Ok(v) = HeaderValue::from_str(kind) {
        res.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}
