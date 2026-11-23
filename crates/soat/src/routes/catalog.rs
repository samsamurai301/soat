//! Catalog: products of the request's tenant.
//!
//! Every query filters on `tenant_id`, so one tenant can never read or change
//! another's products: a foreign id is simply "not found".

use soat_core::Currency;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::tenant::Tenant;

/// A product.
#[model]
#[derive(sqlx::FromRow)]
pub struct Product {
    pub id: i64,
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

pub fn mount(app: App) -> App {
    app.mount((list_products, create_product, get_product))
}

const COLUMNS: &str = "id, sku, title, price_minor, currency";

/// List the tenant's products.
#[get("/api/products", tag = "catalog")]
async fn list_products(db: Db, tenant: Tenant) -> Result<Json<Vec<Product>>> {
    tenant.verify(&db).await?;
    let products = sqlx::query_as::<_, Product>(&format!(
        "SELECT {COLUMNS} FROM products WHERE tenant_id = $1 ORDER BY id"
    ))
    .bind(tenant.id())
    .fetch_all(&*db)
    .await?;
    Ok(Json(products))
}

/// Create a product. A SKU already used by this tenant is a 409.
#[post("/api/products", tag = "catalog", status = 201)]
async fn create_product(
    db: Db,
    tenant: Tenant,
    Valid(Json(p)): Valid<Json<NewProduct>>,
) -> Result<(StatusCode, Json<Product>)> {
    tenant.verify(&db).await?;
    let currency = Currency::parse(&p.currency)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))?;
    let product = sqlx::query_as::<_, Product>(&format!(
        "INSERT INTO products (tenant_id, sku, title, price_minor, currency) \
         VALUES ($1, $2, $3, $4, $5) RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&p.sku)
    .bind(&p.title)
    .bind(p.price_minor)
    .bind(currency.as_str())
    .fetch_one(&*db)
    .await?;
    Ok((StatusCode::CREATED, Json(product)))
}

/// Get one of the tenant's products.
#[get("/api/products/{id}", tag = "catalog")]
async fn get_product(db: Db, tenant: Tenant, Path(id): Path<i64>) -> Result<Json<Product>> {
    tenant.verify(&db).await?;
    sqlx::query_as::<_, Product>(&format!(
        "SELECT {COLUMNS} FROM products WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id)
    .fetch_optional(&*db)
    .await?
    .map(Json)
    .ok_or_else(Error::not_found)
}
