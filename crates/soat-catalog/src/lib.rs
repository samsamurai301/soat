//! Catalog: products of the request's tenant.
//!
//! Variants, collections, categories and media follow (milestones B1 to B12).
//!
//! Every query runs in a tenant transaction (row-level security) and filters
//! on `tenant_id`, so one tenant can never read or change
//! another's products: a foreign id is simply "not found".

use soat_core::{id_kind, Currency};
use soat_module::Module;
use soat_tenancy::{Tenant, TenantListing};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(ProductId, ProductKind, "prod");

/// The catalog module. Needs `tenancy`.
pub struct Catalog;

impl Module for Catalog {
    fn name(&self) -> &'static str {
        "catalog"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn mount(&self, app: App) -> App {
        app.mount((list_products, create_product, get_product))
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
    /// Price in minor units (cents).
    pub price_minor: i64,
    /// ISO 4217 code, e.g. `EUR`.
    pub currency: String,
}

/// Body of a create request.
#[model]
pub struct NewProduct {
    #[validate(length(min = 1, max = 64))]
    pub sku: String,
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    #[validate(range(min = 0))]
    pub price_minor: i64,
    #[validate(length(equal = 3))]
    pub currency: String,
}

const COLUMNS: &str = "id, sku, title, price_minor, currency";

/// List the tenant's products, 50 per page (at most 200).
///
/// `?q=` searches SKU and title, `?currency=EUR` filters, `?sort=-price_minor`
/// sorts (by `id`, `sku`, `title` or `price_minor`), `?limit=&offset=` pages.
/// Ids are time-ordered, so the default sort is oldest first.
#[get("/api/products", tag = "catalog")]
async fn list_products(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Product>>> {
    let mut tx = tenant.begin(&db).await?;
    let page = TenantListing::new("products", COLUMNS)
        .sortable(["id", "sku", "title", "price_minor"])
        .search(["sku", "title"])
        .filter_text("currency")
        .page_size(50, 200)
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a product. A SKU already used by this tenant is a 409.
#[post("/api/products", tag = "catalog", status = 201)]
async fn create_product(
    db: Db,
    tenant: Tenant,
    Valid(Json(p)): Valid<Json<NewProduct>>,
) -> Result<(StatusCode, Json<Product>)> {
    let currency = Currency::parse(&p.currency)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))?;
    let mut tx = tenant.begin(&db).await?;
    let product = sqlx::query_as::<_, Product>(&format!(
        "INSERT INTO products (id, tenant_id, sku, title, price_minor, currency) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
    ))
    .bind(ProductId::new().as_str())
    .bind(tenant.id())
    .bind(&p.sku)
    .bind(&p.title)
    .bind(p.price_minor)
    .bind(currency.as_str())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(product)))
}

/// Get one of the tenant's products. A malformed or foreign id is a 404.
#[get("/api/products/{id}", tag = "catalog")]
async fn get_product(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Product>> {
    let id = ProductId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin(&db).await?;
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
