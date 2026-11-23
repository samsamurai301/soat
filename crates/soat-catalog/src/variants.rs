//! Options and variants of a product.
//!
//! A product without options sells as itself. Options (at most three, e.g.
//! Size and Color) define variants: each picks one value per option and has
//! its own SKU and price, in the product's currency.

use soat_core::id_kind;
use soat_core::options::{check_options, check_variant, variant_title, MAX_VARIANTS};
use soat_tenancy::{Scope, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{ProductId, PRODUCTS_READ, PRODUCTS_WRITE};

id_kind!(VariantId, VariantKind, "var");

/// An option of a product and its values, in order.
#[model]
pub struct ProductOption {
    /// e.g. `Size`.
    pub name: String,
    /// e.g. `["S", "M", "L"]`.
    pub values: Vec<String>,
}

/// Body of `PUT /api/products/{id}/options`.
#[model]
pub struct SetOptions {
    pub options: Vec<ProductOption>,
}

/// A variant of a product.
#[model]
pub struct Variant {
    /// e.g. `var_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub product_id: String,
    pub sku: String,
    /// The values joined, e.g. `M / Red`.
    pub title: String,
    /// One value per option, in option order.
    pub options: Vec<String>,
    /// Price in minor units of `currency`.
    pub price_minor: i64,
    pub currency: String,
}

/// Body of a variant create request.
#[model]
pub struct NewVariant {
    #[validate(length(min = 1, max = 64))]
    pub sku: String,
    /// One value per option, in option order.
    pub options: Vec<String>,
    /// Defaults to the product's price.
    #[validate(range(min = 0))]
    pub price_minor: Option<i64>,
}

/// Body of a variant update; absent fields stay as they are.
#[model]
pub struct VariantChange {
    #[validate(length(min = 1, max = 64))]
    pub sku: Option<String>,
    #[validate(range(min = 0))]
    pub price_minor: Option<i64>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        get_options,
        set_options,
        list_variants,
        create_variant,
        update_variant,
        delete_variant,
    ))
}

type Options = Vec<(String, Vec<String>)>;

#[derive(sqlx::FromRow)]
struct VariantRow {
    id: String,
    product_id: String,
    sku: String,
    option_values: Vec<String>,
    price_minor: i64,
    currency: String,
}

impl From<VariantRow> for Variant {
    fn from(r: VariantRow) -> Self {
        Self {
            title: variant_title(&r.option_values),
            id: r.id,
            product_id: r.product_id,
            sku: r.sku,
            options: r.option_values,
            price_minor: r.price_minor,
            currency: r.currency,
        }
    }
}

const VARIANT_COLUMNS: &str =
    "v.id, v.product_id, v.sku, v.option_values, v.price_minor, p.currency";

/// The product's `(price_minor, currency)`, or 404 for a malformed or
/// foreign id.
async fn product(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<(String, i64)> {
    let id = ProductId::parse(id).map_err(|_| Error::not_found())?;
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT id, price_minor FROM products WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    row.ok_or_else(Error::not_found)
}

pub(crate) async fn load_options(
    tx: &mut PgConnection,
    tenant: &impl Scope,
    product_id: &str,
) -> Result<Options> {
    Ok(sqlx::query_as(
        "SELECT name, option_values FROM product_options \
         WHERE tenant_id = $1 AND product_id = $2 ORDER BY position",
    )
    .bind(tenant.tenant_id())
    .bind(product_id)
    .fetch_all(&mut *tx)
    .await?)
}

pub(crate) async fn load_variants(
    tx: &mut PgConnection,
    tenant: &impl Scope,
    product_id: &str,
) -> Result<Vec<Variant>> {
    let rows: Vec<VariantRow> = sqlx::query_as(&format!(
        "SELECT {VARIANT_COLUMNS} FROM product_variants v \
         JOIN products p ON p.id = v.product_id \
         WHERE v.tenant_id = $1 AND v.product_id = $2 ORDER BY v.id"
    ))
    .bind(tenant.tenant_id())
    .bind(product_id)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows.into_iter().map(Variant::from).collect())
}

async fn load_variant(
    tx: &mut TenantTx,
    tenant: &Tenant,
    product_id: &str,
    id: &str,
) -> Result<Variant> {
    let id = VariantId::parse(id).map_err(|_| Error::not_found())?;
    let row: Option<VariantRow> = sqlx::query_as(&format!(
        "SELECT {VARIANT_COLUMNS} FROM product_variants v \
         JOIN products p ON p.id = v.product_id \
         WHERE v.tenant_id = $1 AND v.product_id = $2 AND v.id = $3"
    ))
    .bind(tenant.id())
    .bind(product_id)
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(Variant::from).ok_or_else(Error::not_found)
}

fn invalid(e: impl std::fmt::Display) -> Error {
    Error::unprocessable(e.to_string()).with_code("invalid_options")
}

fn to_views(options: Options) -> Vec<ProductOption> {
    options
        .into_iter()
        .map(|(name, values)| ProductOption { name, values })
        .collect()
}

/// The product's options, in order (`products:read`).
#[get("/api/products/{id}/options", tag = "catalog")]
async fn get_options(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Vec<ProductOption>>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let (product_id, _) = product(&mut tx, &tenant, &id).await?;
    let options = load_options(&mut tx, &tenant, &product_id).await?;
    tx.commit().await?;
    Ok(Json(to_views(options)))
}

/// Replace the product's options (`products:write`): at most three, each
/// with distinct values. Every existing variant must still fit (add values
/// freely; remove a value or option only once no variant uses it), else
/// 409 `variants_do_not_fit`. Publishes `product.options_changed`.
#[put("/api/products/{id}/options", tag = "catalog")]
async fn set_options(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(body): Json<SetOptions>,
) -> Result<Json<Vec<ProductOption>>> {
    let options: Options = body
        .options
        .into_iter()
        .map(|o| (o.name, o.values))
        .collect();
    check_options(&options).map_err(invalid)?;
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let (product_id, _) = product(&mut tx, &tenant, &id).await?;
    for variant in load_variants(&mut tx, &tenant, &product_id).await? {
        if let Err(e) = check_variant(&options, &variant.options) {
            return Err(
                Error::conflict(format!("variant {} would not fit: {e}", variant.sku))
                    .with_code("variants_do_not_fit"),
            );
        }
    }
    sqlx::query("DELETE FROM product_options WHERE tenant_id = $1 AND product_id = $2")
        .bind(tenant.id())
        .bind(&product_id)
        .execute(&mut *tx)
        .await?;
    for (position, (name, values)) in options.iter().enumerate() {
        sqlx::query(
            "INSERT INTO product_options (tenant_id, product_id, position, name, option_values) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tenant.id())
        .bind(&product_id)
        .bind(position as i32)
        .bind(name)
        .bind(values)
        .execute(&mut *tx)
        .await?;
    }
    let views = to_views(options);
    tenant
        .publish(
            &mut tx,
            "product.options_changed",
            &serde_json::json!({ "product_id": product_id, "options": views }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(views))
}

/// The product's variants, oldest first (`products:read`).
#[get("/api/products/{id}/variants", tag = "catalog")]
async fn list_variants(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Vec<Variant>>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let (product_id, _) = product(&mut tx, &tenant, &id).await?;
    let variants = load_variants(&mut tx, &tenant, &product_id).await?;
    tx.commit().await?;
    Ok(Json(variants))
}

/// Add a variant (`products:write`): one value per option, in order. A SKU
/// this shop already uses on a variant, or values another variant has, is
/// a 409. Publishes `variant.created`.
#[post("/api/products/{id}/variants", tag = "catalog", status = 201)]
async fn create_variant(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(body)): Valid<Json<NewVariant>>,
) -> Result<(StatusCode, Json<Variant>)> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let (product_id, product_price) = product(&mut tx, &tenant, &id).await?;
    let options = load_options(&mut tx, &tenant, &product_id).await?;
    check_variant(&options, &body.options)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_variant"))?;
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM product_variants WHERE tenant_id = $1 AND product_id = $2",
    )
    .bind(tenant.id())
    .bind(&product_id)
    .fetch_one(&mut *tx)
    .await?;
    if count >= MAX_VARIANTS as i64 {
        return Err(
            Error::conflict(format!("a product has at most {MAX_VARIANTS} variants"))
                .with_code("too_many_variants"),
        );
    }
    let variant_id = VariantId::new();
    sqlx::query(
        "INSERT INTO product_variants (id, tenant_id, product_id, sku, price_minor, option_values) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(variant_id.as_str())
    .bind(tenant.id())
    .bind(&product_id)
    .bind(&body.sku)
    .bind(body.price_minor.unwrap_or(product_price))
    .bind(&body.options)
    .execute(&mut *tx)
    .await?;
    let variant = load_variant(&mut tx, &tenant, &product_id, variant_id.as_str()).await?;
    tenant.publish(&mut tx, "variant.created", &variant).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(variant)))
}

/// Change a variant's SKU or price (`products:write`). Publishes
/// `variant.updated`.
#[patch("/api/products/{id}/variants/{variant_id}", tag = "catalog")]
async fn update_variant(
    db: Db,
    tenant: Tenant,
    Path((id, variant_id)): Path<(String, String)>,
    Valid(Json(change)): Valid<Json<VariantChange>>,
) -> Result<Json<Variant>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let (product_id, _) = product(&mut tx, &tenant, &id).await?;
    let current = load_variant(&mut tx, &tenant, &product_id, &variant_id).await?;
    sqlx::query(
        "UPDATE product_variants SET sku = $1, price_minor = $2 \
         WHERE tenant_id = $3 AND id = $4",
    )
    .bind(change.sku.as_ref().unwrap_or(&current.sku))
    .bind(change.price_minor.unwrap_or(current.price_minor))
    .bind(tenant.id())
    .bind(&current.id)
    .execute(&mut *tx)
    .await?;
    let variant = load_variant(&mut tx, &tenant, &product_id, &current.id).await?;
    tenant.publish(&mut tx, "variant.updated", &variant).await?;
    tx.commit().await?;
    Ok(Json(variant))
}

/// Delete a variant (`products:write`). Publishes `variant.deleted`.
#[delete(
    "/api/products/{id}/variants/{variant_id}",
    tag = "catalog",
    status = 204
)]
async fn delete_variant(
    db: Db,
    tenant: Tenant,
    Path((id, variant_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let (product_id, _) = product(&mut tx, &tenant, &id).await?;
    let variant = load_variant(&mut tx, &tenant, &product_id, &variant_id).await?;
    sqlx::query("DELETE FROM product_variants WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&variant.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "variant.deleted",
            &serde_json::json!({ "id": variant.id, "product_id": product_id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
