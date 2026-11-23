//! Collections: hand-picked groups of products in the merchant's order
//! (`summer-sale`, `new-in`). Rule-based collections come in B3.

use soat_core::handle::{handleize, is_handle};
use soat_core::{id_kind, Permission};
use soat_tenancy::{Tenant, TenantListing, TenantTx};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{Product, ProductId};

id_kind!(CollectionId, CollectionKind, "col");

/// See collections.
pub const COLLECTIONS_READ: Permission =
    Permission::new("collections:read", "See collections").for_staff();
/// Create, change and fill collections.
pub const COLLECTIONS_WRITE: Permission =
    Permission::new("collections:write", "Create, change and fill collections").for_staff();

/// A collection.
#[model]
#[derive(sqlx::FromRow)]
pub struct Collection {
    /// e.g. `col_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// The URL slug, unique in the shop, e.g. `summer-sale`.
    pub handle: String,
    pub title: String,
    pub description: String,
    /// The title search engines show; empty uses the title.
    pub seo_title: String,
    /// The description search engines show; empty uses the description.
    pub seo_description: String,
}

/// Body of a create request.
#[model]
pub struct NewCollection {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    /// Made from the title when absent.
    pub handle: Option<String>,
    #[validate(length(max = 20000))]
    pub description: Option<String>,
    #[validate(length(max = 200))]
    pub seo_title: Option<String>,
    #[validate(length(max = 500))]
    pub seo_description: Option<String>,
}

/// Body of an update request; absent fields stay as they are.
#[model]
pub struct CollectionChange {
    #[validate(length(min = 1, max = 200))]
    pub title: Option<String>,
    pub handle: Option<String>,
    #[validate(length(max = 20000))]
    pub description: Option<String>,
    #[validate(length(max = 200))]
    pub seo_title: Option<String>,
    #[validate(length(max = 500))]
    pub seo_description: Option<String>,
}

/// Body of a reorder request: every product of the collection, in the new
/// order.
#[model]
pub struct ProductOrder {
    pub product_ids: Vec<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_collections,
        create_collection,
        get_collection,
        update_collection,
        delete_collection,
    ))
    .mount((
        list_collection_products,
        add_product,
        remove_product,
        reorder_products,
    ))
}

const COLUMNS: &str = "id, handle, title, description, seo_title, seo_description";

fn check_handle(handle: &str) -> Result<()> {
    if is_handle(handle) {
        Ok(())
    } else {
        Err(Error::unprocessable(
            "a handle is 1-100 of a-z, 0-9 and single hyphens, e.g. summer-sale",
        )
        .with_code("invalid_handle"))
    }
}

async fn load(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<Collection> {
    let id = CollectionId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Collection>(&format!(
        "SELECT {COLUMNS} FROM collections WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's collections, 50 per page. `?q=` searches title and handle;
/// `?sort=` by `id`, `title` or `handle`.
#[get("/api/collections", tag = "collections")]
async fn list_collections(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Collection>>> {
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_READ).await?;
    let page = TenantListing::new("collections", COLUMNS)
        .sortable(["title", "handle"])
        .search(["title", "handle"])
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a collection (`collections:write`). A handle the shop already
/// uses is a 409. Publishes `collection.created`.
#[post("/api/collections", tag = "collections", status = 201)]
async fn create_collection(
    db: Db,
    tenant: Tenant,
    Valid(Json(body)): Valid<Json<NewCollection>>,
) -> Result<(StatusCode, Json<Collection>)> {
    let handle = body.handle.unwrap_or_else(|| handleize(&body.title));
    check_handle(&handle)?;
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_WRITE).await?;
    let collection = sqlx::query_as::<_, Collection>(&format!(
        "INSERT INTO collections (id, tenant_id, handle, title, description, seo_title, \
         seo_description) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {COLUMNS}"
    ))
    .bind(CollectionId::new().as_str())
    .bind(tenant.id())
    .bind(&handle)
    .bind(&body.title)
    .bind(body.description.unwrap_or_default())
    .bind(body.seo_title.as_deref().unwrap_or_default().trim())
    .bind(body.seo_description.as_deref().unwrap_or_default().trim())
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "collection.created", &collection)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(collection)))
}

/// One collection (`collections:read`).
#[get("/api/collections/{id}", tag = "collections")]
async fn get_collection(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Collection>> {
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_READ).await?;
    let collection = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(collection))
}

/// Change a collection (`collections:write`). Publishes
/// `collection.updated`, and `collection.handle_changed` (`id`, `from`,
/// `to`) when the handle changes, so old links can redirect.
#[patch("/api/collections/{id}", tag = "collections")]
async fn update_collection(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(change)): Valid<Json<CollectionChange>>,
) -> Result<Json<Collection>> {
    if let Some(handle) = &change.handle {
        check_handle(handle)?;
    }
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    let collection = sqlx::query_as::<_, Collection>(&format!(
        "UPDATE collections SET handle = COALESCE($3, handle), title = COALESCE($4, title), \
         description = COALESCE($5, description), seo_title = COALESCE($6, seo_title), \
         seo_description = COALESCE($7, seo_description) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(&change.handle)
    .bind(&change.title)
    .bind(&change.description)
    .bind(change.seo_title.as_deref().map(str::trim))
    .bind(change.seo_description.as_deref().map(str::trim))
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "collection.updated", &collection)
        .await?;
    if current.handle != collection.handle {
        tenant
            .publish(
                &mut tx,
                "collection.handle_changed",
                &serde_json::json!({
                    "id": collection.id, "from": current.handle, "to": collection.handle
                }),
            )
            .await?;
    }
    tx.commit().await?;
    Ok(Json(collection))
}

/// Delete a collection; its products stay (`collections:write`).
/// Publishes `collection.deleted`.
#[delete("/api/collections/{id}", tag = "collections", status = 204)]
async fn delete_collection(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM collections WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&current.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(&mut tx, "collection.deleted", &current)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The collection's products in the merchant's order, 50 per page
/// (`collections:read` and `products:read`). `?sort=` by `position`,
/// `title` or `price_minor`; `?q=` searches SKU and title.
#[get("/api/collections/{id}/products", tag = "collections")]
async fn list_collection_products(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    params: ListParams,
) -> Result<Json<Paged<Product>>> {
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&COLLECTIONS_READ)?;
    access.require(&crate::PRODUCTS_READ)?;
    let collection = load(&mut tx, &tenant, &id).await?;
    let page = TenantListing::new(
        "(SELECT p.id, p.sku, p.title, p.handle, p.description, p.status, p.price_minor, \
          p.currency, p.tags, p.tax_class, p.weight_grams, p.seo_title, p.seo_description, \
          cp.collection_id, cp.position \
          FROM products p JOIN collection_products cp ON cp.product_id = p.id) AS items",
        crate::COLUMNS,
    )
    .scope_text("collection_id", collection.id)
    .sortable(["position", "title", "price_minor"])
    .default_sort("position")
    .search(["sku", "title"])
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Add a product at the end of the collection (`collections:write`).
/// Adding it twice changes nothing. Publishes `collection.product_added`.
#[put(
    "/api/collections/{id}/products/{product_id}",
    tag = "collections",
    status = 204
)]
async fn add_product(
    db: Db,
    tenant: Tenant,
    Path((id, product_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_WRITE).await?;
    let collection = load(&mut tx, &tenant, &id).await?;
    let product_id = ProductId::parse(&product_id).map_err(|_| Error::not_found())?;
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM products WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(product_id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    if exists.is_none() {
        return Err(Error::not_found());
    }
    let added = sqlx::query(
        "INSERT INTO collection_products (tenant_id, collection_id, product_id, position) \
         SELECT $1, $2, $3, COALESCE(MAX(position) + 1, 0) FROM collection_products \
         WHERE tenant_id = $1 AND collection_id = $2 \
         ON CONFLICT (collection_id, product_id) DO NOTHING",
    )
    .bind(tenant.id())
    .bind(&collection.id)
    .bind(product_id.as_str())
    .execute(&mut *tx)
    .await?;
    if added.rows_affected() > 0 {
        tenant
            .publish(
                &mut tx,
                "collection.product_added",
                &serde_json::json!({ "collection_id": collection.id, "product_id": product_id.as_str() }),
            )
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Take a product out of the collection (`collections:write`). Publishes
/// `collection.product_removed`.
#[delete(
    "/api/collections/{id}/products/{product_id}",
    tag = "collections",
    status = 204
)]
async fn remove_product(
    db: Db,
    tenant: Tenant,
    Path((id, product_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_WRITE).await?;
    let collection = load(&mut tx, &tenant, &id).await?;
    let removed = sqlx::query(
        "DELETE FROM collection_products \
         WHERE tenant_id = $1 AND collection_id = $2 AND product_id = $3",
    )
    .bind(tenant.id())
    .bind(&collection.id)
    .bind(&product_id)
    .execute(&mut *tx)
    .await?;
    if removed.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "collection.product_removed",
            &serde_json::json!({ "collection_id": collection.id, "product_id": product_id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Set the order of the collection's products (`collections:write`). The
/// list must name every product of the collection exactly once, else 422
/// `order_mismatch`. Publishes `collection.reordered`.
#[put("/api/collections/{id}/order", tag = "collections", status = 204)]
async fn reorder_products(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(order): Json<ProductOrder>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &COLLECTIONS_WRITE).await?;
    let collection = load(&mut tx, &tenant, &id).await?;
    let mut members: Vec<(String,)> = sqlx::query_as(
        "SELECT product_id FROM collection_products WHERE tenant_id = $1 AND collection_id = $2",
    )
    .bind(tenant.id())
    .bind(&collection.id)
    .fetch_all(&mut *tx)
    .await?;
    let mut wanted = order.product_ids.clone();
    members.sort();
    wanted.sort();
    if members.into_iter().map(|(id,)| id).ne(wanted) {
        return Err(
            Error::unprocessable("list every product of the collection exactly once")
                .with_code("order_mismatch"),
        );
    }
    sqlx::query(
        "UPDATE collection_products cp SET position = o.position - 1 \
         FROM unnest($3::text[]) WITH ORDINALITY AS o(product_id, position) \
         WHERE cp.tenant_id = $1 AND cp.collection_id = $2 AND cp.product_id = o.product_id",
    )
    .bind(tenant.id())
    .bind(&collection.id)
    .bind(&order.product_ids)
    .execute(&mut *tx)
    .await?;
    tenant
        .publish(
            &mut tx,
            "collection.reordered",
            &serde_json::json!({ "collection_id": collection.id, "product_ids": order.product_ids }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
