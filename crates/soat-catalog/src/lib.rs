//! Catalog: products of the request's tenant.
//!
//! Products have options and variants ([`variants`]) and are grouped in
//! [`collections`], sorted into [`categories`] and tagged. Media and the
//! rest follow (milestones B5 to B12).
//!
//! Every query runs in a tenant transaction (row-level security) and filters
//! on `tenant_id`, so one tenant can never read or change
//! another's products: a foreign id is simply "not found".

pub mod categories;
pub mod collections;
pub mod media;
pub mod search;
pub mod storefront;
pub mod variants;

use soat_core::tags::normalize_tags;
use soat_core::{id_kind, Currency, Permission};
use soat_module::Module;
use soat_tenancy::{Tenant, TenantListing};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(ProductId, ProductKind, "prod");

/// See products.
pub const PRODUCTS_READ: Permission = Permission::new("products:read", "See products").for_staff();
/// Create and change products.
pub const PRODUCTS_WRITE: Permission =
    Permission::new("products:write", "Create and change products").for_staff();

/// The catalog module. Needs `tenancy`.
pub struct Catalog;

impl Module for Catalog {
    fn name(&self) -> &'static str {
        "catalog"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "fields", "files"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[
            PRODUCTS_READ,
            PRODUCTS_WRITE,
            collections::COLLECTIONS_READ,
            collections::COLLECTIONS_WRITE,
            categories::CATEGORIES_READ,
            categories::CATEGORIES_WRITE,
        ]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_products,
            create_product,
            get_product,
            update_product,
            delete_product,
            get_product_fields,
            set_product_fields,
        ))
        .plugin(variants::mount)
        .plugin(collections::mount)
        .plugin(categories::mount)
        .plugin(media::mount)
        .plugin(storefront::mount)
        .plugin(search::mount)
    }
}

/// A product.
#[model]
#[derive(sqlx::FromRow)]
pub struct Product {
    /// Public id, e.g. `prod_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub sku: String,
    pub title: String,
    /// Its address on the storefront: `/products/<handle>`.
    pub handle: String,
    /// Shown on the product page, in Markdown.
    pub description: String,
    /// `active` (sold on the storefront), `draft` or `archived`.
    pub status: String,
    /// Price in minor units (cents).
    pub price_minor: i64,
    /// ISO 4217 code, e.g. `EUR`.
    pub currency: String,
    /// Free-form tags, e.g. `["summer", "organic"]`.
    pub tags: Vec<String>,
    /// Which tax rates apply, e.g. `standard` or `reduced`; see the tax
    /// zones.
    pub tax_class: String,
    /// Shipping weight in grams; its variants share it.
    pub weight_grams: Option<i64>,
    /// The title search engines show; empty uses the title.
    pub seo_title: String,
    /// The description search engines show; empty uses the start of the
    /// description.
    pub seo_description: String,
}

/// Body of a create request.
#[model]
pub struct NewProduct {
    #[validate(length(min = 1, max = 64))]
    pub sku: String,
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    /// Made from the title when absent.
    pub handle: Option<String>,
    /// Markdown.
    #[validate(length(max = 50000))]
    pub description: Option<String>,
    /// `active` (the default), `draft` or `archived`.
    pub status: Option<String>,
    #[validate(range(min = 0))]
    pub price_minor: i64,
    #[validate(length(equal = 3))]
    pub currency: String,
    /// Trimmed, deduplicated; at most 50.
    pub tags: Option<Vec<String>>,
    /// Defaults to `standard`; `exempt` is never taxed.
    pub tax_class: Option<String>,
    #[validate(range(min = 0, max = 10_000_000))]
    pub weight_grams: Option<i64>,
    #[validate(length(max = 200))]
    pub seo_title: Option<String>,
    #[validate(length(max = 500))]
    pub seo_description: Option<String>,
}

/// Body of an update request; absent fields stay as they are. The currency
/// is fixed once a product exists.
#[model]
pub struct ProductChange {
    #[validate(length(min = 1, max = 64))]
    pub sku: Option<String>,
    #[validate(length(min = 1, max = 200))]
    pub title: Option<String>,
    pub handle: Option<String>,
    #[validate(length(max = 50000))]
    pub description: Option<String>,
    pub status: Option<String>,
    #[validate(range(min = 0))]
    pub price_minor: Option<i64>,
    /// Replaces all tags.
    pub tags: Option<Vec<String>>,
    pub tax_class: Option<String>,
    #[validate(range(min = 0, max = 10_000_000))]
    pub weight_grams: Option<i64>,
    /// An empty string goes back to the title.
    #[validate(length(max = 200))]
    pub seo_title: Option<String>,
    #[validate(length(max = 500))]
    pub seo_description: Option<String>,
}

fn tax_class(raw: Option<&str>) -> Result<()> {
    match raw {
        Some(c) if !soat_core::tax::is_tax_class(c) => Err(Error::unprocessable(
            "a tax class is 1 to 32 of a-z, 0-9, - and _, starting with a letter",
        )
        .with_code("invalid_tax_class")),
        _ => Ok(()),
    }
}

fn tags(raw: Option<&[String]>) -> Result<Option<Vec<String>>> {
    raw.map(|t| normalize_tags(t).map_err(|e| Error::unprocessable(e).with_code("invalid_tags")))
        .transpose()
}

const COLUMNS: &str = "id, sku, title, handle, description, status, price_minor, currency, tags, \
     tax_class, weight_grams, seo_title, seo_description";

/// Product statuses.
pub const STATUSES: &[&str] = &["active", "draft", "archived"];

fn status(raw: Option<&str>) -> Result<()> {
    match raw {
        Some(s) if !STATUSES.contains(&s) => {
            Err(Error::unprocessable("status is active, draft or archived")
                .with_code("invalid_status"))
        }
        _ => Ok(()),
    }
}

fn handle(raw: Option<&str>) -> Result<()> {
    match raw {
        Some(h) if !soat_core::handle::is_handle(h) => Err(Error::unprocessable(format!(
            "{h} is not a handle: lowercase letters, digits and single dashes"
        ))
        .with_code("invalid_handle")),
        _ => Ok(()),
    }
}

/// A free handle for a new product titled `title`: the title's, or with a
/// piece of the id when another product has it.
async fn free_handle(
    tx: &mut soat_tenancy::TenantTx,
    tenant: &Tenant,
    title: &str,
    id: &str,
) -> Result<String> {
    let base = soat_core::handle::handleize(title);
    let taken: Option<(String,)> =
        sqlx::query_as("SELECT id FROM products WHERE tenant_id = $1 AND handle = $2")
            .bind(tenant.id())
            .bind(&base)
            .fetch_optional(&mut **tx)
            .await?;
    if taken.is_none() {
        return Ok(base);
    }
    let tail: String = id
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let mut base = base;
    base.truncate(soat_core::handle::MAX_HANDLE - 9);
    Ok(format!(
        "{}-{}",
        base.trim_end_matches('-'),
        tail.to_ascii_lowercase()
    ))
}

/// List the tenant's products, 50 per page (at most 200).
///
/// `?q=` searches SKU, title and handle; `?currency=EUR`, `?status=draft` and `?tag=summer` filter, `?sort=-price_minor`
/// sorts (by `id`, `sku`, `title` or `price_minor`), `?limit=&offset=` pages.
/// Ids are time-ordered, so the default sort is oldest first.
#[get("/api/products", tag = "catalog")]
async fn list_products(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Product>>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let page = TenantListing::new("products", COLUMNS)
        .sortable(["id", "sku", "title", "price_minor"])
        .search(["sku", "title", "handle"])
        .filter_text("currency")
        .filter_text("status")
        .filter_array("tag", "tags")
        .page_size(50, 200)
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a product (`products:write`). A SKU already used by this shop is a
/// 409. Publishes `product.created`.
#[post("/api/products", tag = "catalog", status = 201)]
async fn create_product(
    db: Db,
    tenant: Tenant,
    Valid(Json(p)): Valid<Json<NewProduct>>,
) -> Result<(StatusCode, Json<Product>)> {
    let currency = Currency::parse(&p.currency)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))?;
    let tags = tags(p.tags.as_deref())?.unwrap_or_default();
    tax_class(p.tax_class.as_deref())?;
    status(p.status.as_deref())?;
    handle(p.handle.as_deref())?;
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let id = ProductId::new();
    let handle = match &p.handle {
        Some(h) => h.clone(),
        None => free_handle(&mut tx, &tenant, &p.title, id.as_str()).await?,
    };
    let product = sqlx::query_as::<_, Product>(&format!(
        "INSERT INTO products \
         (id, tenant_id, sku, title, price_minor, currency, tags, tax_class, weight_grams, \
          handle, description, status, seo_title, seo_description) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING {COLUMNS}"
    ))
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(&p.sku)
    .bind(&p.title)
    .bind(p.price_minor)
    .bind(currency.as_str())
    .bind(&tags)
    .bind(p.tax_class.as_deref().unwrap_or(soat_core::tax::STANDARD))
    .bind(p.weight_grams)
    .bind(&handle)
    .bind(p.description.as_deref().unwrap_or_default())
    .bind(p.status.as_deref().unwrap_or("active"))
    .bind(p.seo_title.as_deref().unwrap_or_default().trim())
    .bind(p.seo_description.as_deref().unwrap_or_default().trim())
    .fetch_one(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "product.created", &product).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(product)))
}

/// Get one of the tenant's products. A malformed or foreign id is a 404.
#[get("/api/products/{id}", tag = "catalog")]
async fn get_product(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Product>> {
    let id = ProductId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let product = sqlx::query_as::<_, Product>(&format!(
        "SELECT {COLUMNS} FROM products WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    product.map(Json).ok_or_else(Error::not_found)
}

/// Change a product's SKU, title, handle, description, status, price, tags,
/// tax class or SEO title and description (`products:write`). A handle
/// another product has is a 409. Publishes `product.updated`, and
/// `product.handle_changed` (`id`, `from`, `to`) when the handle changes, so
/// old links can redirect.
#[patch("/api/products/{id}", tag = "catalog")]
async fn update_product(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(change)): Valid<Json<ProductChange>>,
) -> Result<Json<Product>> {
    let id = ProductId::parse(&id).map_err(|_| Error::not_found())?;
    let tags = tags(change.tags.as_deref())?;
    tax_class(change.tax_class.as_deref())?;
    status(change.status.as_deref())?;
    handle(change.handle.as_deref())?;
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let before: Option<(String,)> =
        sqlx::query_as("SELECT handle FROM products WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
            .bind(tenant.id())
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    let (before,) = before.ok_or_else(Error::not_found)?;
    let product = sqlx::query_as::<_, Product>(&format!(
        "UPDATE products SET sku = COALESCE($3, sku), title = COALESCE($4, title), \
         handle = COALESCE($9, handle), description = COALESCE($10, description), \
         status = COALESCE($11, status), \
         price_minor = COALESCE($5, price_minor), tags = COALESCE($6, tags), \
         tax_class = COALESCE($7, tax_class), \
         weight_grams = COALESCE($8, weight_grams), \
         seo_title = COALESCE($12, seo_title), \
         seo_description = COALESCE($13, seo_description) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .bind(&change.sku)
    .bind(&change.title)
    .bind(change.price_minor)
    .bind(&tags)
    .bind(&change.tax_class)
    .bind(change.weight_grams)
    .bind(&change.handle)
    .bind(&change.description)
    .bind(&change.status)
    .bind(change.seo_title.as_deref().map(str::trim))
    .bind(change.seo_description.as_deref().map(str::trim))
    .fetch_one(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "product.updated", &product).await?;
    if before != product.handle {
        tenant
            .publish(
                &mut tx,
                "product.handle_changed",
                &serde_json::json!({ "id": product.id, "from": before, "to": product.handle }),
            )
            .await?;
    }
    tx.commit().await?;
    Ok(Json(product))
}

/// Delete a product with its options and variants (`products:write`).
/// Publishes `product.deleted` with the ids of its variants.
#[delete("/api/products/{id}", tag = "catalog", status = 204)]
async fn delete_product(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = ProductId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let variants: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM product_variants WHERE tenant_id = $1 AND product_id = $2 ORDER BY id",
    )
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_all(&mut *tx)
    .await?;
    let variant_ids: Vec<String> = variants.into_iter().map(|(v,)| v).collect();
    let deleted = sqlx::query("DELETE FROM products WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    soat_fields::delete_values(&mut tx, &tenant, "product", id.as_str()).await?;
    tenant
        .publish(
            &mut tx,
            "product.deleted",
            &serde_json::json!({ "id": id.as_str(), "variant_ids": variant_ids }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The product's custom field values, by key (`products:read`). Define
/// fields with `PUT /api/fields/product/{key}`.
#[get("/api/products/{id}/fields", tag = "catalog")]
async fn get_product_fields(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Map<String, serde_json::Value>>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let id = existing(&mut tx, &tenant, &id).await?;
    let values = soat_fields::values(&mut tx, &tenant, "product", &id).await?;
    tx.commit().await?;
    Ok(Json(values))
}

/// Set some of the product's custom field values; `null` clears one
/// (`products:write`). Publishes `product.fields_changed` with every value.
#[patch("/api/products/{id}/fields", tag = "catalog")]
async fn set_product_fields(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(changes): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<Json<serde_json::Map<String, serde_json::Value>>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let id = existing(&mut tx, &tenant, &id).await?;
    let values = soat_fields::set_values(&mut tx, &tenant, "product", &id, &changes).await?;
    tenant
        .publish(
            &mut tx,
            "product.fields_changed",
            &serde_json::json!({ "product_id": id, "fields": values }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(values))
}

/// The id of one of the shop's products, or 404.
async fn existing(tx: &mut soat_tenancy::TenantTx, tenant: &Tenant, id: &str) -> Result<String> {
    let id = ProductId::parse(id).map_err(|_| Error::not_found())?;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT id FROM products WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(id,)| id).ok_or_else(Error::not_found)
}

/// Something that can be stocked, priced and sold: a variant, or a
/// product that has no variants.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StockItem {
    /// `var_...` or `prod_...`.
    pub id: String,
    pub product_id: String,
    pub sku: String,
    /// What a buyer sees: the product title, with the variant's values
    /// (`Tee - M / Red`).
    pub title: String,
    /// Its own price, in minor units of `currency`.
    pub price_minor: i64,
    pub currency: String,
    /// Its product's tax class.
    pub tax_class: String,
    /// Its product's weight in grams; 0 when not set.
    pub weight_grams: i64,
}

/// Looks up a stock item of the shop. A product that has variants is not
/// one (`Err` 422 `has_variants`: stock its variants); an unknown id is
/// `Ok(None)`. Run it in the tenant transaction.
pub async fn stock_item(
    conn: &mut sqlx::PgConnection,
    scope: &impl soat_tenancy::Scope,
    id: &str,
) -> Result<Option<StockItem>> {
    if let Ok(vid) = variants::VariantId::parse(id) {
        type VariantRow = (
            String,
            String,
            i64,
            String,
            String,
            Vec<String>,
            String,
            i64,
        );
        let row: Option<VariantRow> = sqlx::query_as(
            "SELECT v.product_id, v.sku, v.price_minor, p.currency, p.title, v.option_values, \
             p.tax_class, COALESCE(p.weight_grams, 0) \
             FROM product_variants v \
             JOIN products p ON p.id = v.product_id WHERE v.tenant_id = $1 AND v.id = $2",
        )
        .bind(scope.tenant_id())
        .bind(vid.as_str())
        .fetch_optional(&mut *conn)
        .await?;
        return Ok(row.map(
            |(product_id, sku, price_minor, currency, title, values, tax_class, weight_grams)| {
                StockItem {
                    id: vid.to_string(),
                    product_id,
                    sku,
                    title: format!("{title} - {}", soat_core::options::variant_title(&values)),
                    price_minor,
                    currency,
                    tax_class,
                    weight_grams,
                }
            },
        ));
    }
    let Ok(pid) = ProductId::parse(id) else {
        return Ok(None);
    };
    let row: Option<(String, String, i64, String, String, i64, bool)> = sqlx::query_as(
        "SELECT sku, title, price_minor, currency, tax_class, COALESCE(weight_grams, 0), \
         EXISTS (SELECT 1 FROM product_variants v WHERE v.product_id = p.id) \
         FROM products p WHERE tenant_id = $1 AND id = $2",
    )
    .bind(scope.tenant_id())
    .bind(pid.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        None => Ok(None),
        Some((.., true)) => Err(Error::unprocessable(
            "this product has variants: stock each variant instead",
        )
        .with_code("has_variants")),
        Some((sku, title, price_minor, currency, tax_class, weight_grams, false)) => {
            Ok(Some(StockItem {
                id: pid.to_string(),
                product_id: pid.to_string(),
                sku,
                title,
                price_minor,
                currency,
                tax_class,
                weight_grams,
            }))
        }
    }
}
