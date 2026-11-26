//! Categories: the shop's tree of product types (Clothing > Shirts >
//! Polos). A product can be in several categories; listing a category's
//! products includes its whole subtree.

use soat_core::handle::{handleize, is_handle};
use soat_core::{id_kind, Permission};
use soat_tenancy::{Tenant, TenantListing, TenantTx};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{Product, ProductId, PRODUCTS_READ, PRODUCTS_WRITE};

id_kind!(CategoryId, CategoryKind, "cat");

/// Deepest a tree may go: a root category is depth 1.
pub const MAX_DEPTH: usize = 8;

/// See categories.
pub const CATEGORIES_READ: Permission =
    Permission::new("categories:read", "See categories").for_staff();
/// Create, move and delete categories.
pub const CATEGORIES_WRITE: Permission =
    Permission::new("categories:write", "Create, move and delete categories").for_staff();

/// A category.
#[model]
#[derive(sqlx::FromRow)]
pub struct Category {
    /// e.g. `cat_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// `null` for a root category.
    pub parent_id: Option<String>,
    /// The ids from the root down to this category.
    pub ancestors: Vec<String>,
    /// The URL slug, unique in the shop.
    pub handle: String,
    pub title: String,
}

/// Body of a create request.
#[model]
pub struct NewCategory {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    /// Made from the title when absent.
    pub handle: Option<String>,
    /// The parent category; absent for a root.
    pub parent_id: Option<String>,
}

/// Body of an update request; absent fields stay as they are.
#[model]
pub struct CategoryChange {
    #[validate(length(min = 1, max = 200))]
    pub title: Option<String>,
    pub handle: Option<String>,
    /// Move under this category.
    pub parent_id: Option<String>,
    /// Move to the root; wins over `parent_id`.
    #[serde(default)]
    pub make_root: bool,
}

/// The categories a product is in.
#[model]
pub struct ProductCategories {
    pub category_ids: Vec<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_categories,
        create_category,
        get_category,
        update_category,
        delete_category,
        list_category_products,
    ))
    .mount((get_product_categories, set_product_categories))
}

const COLUMNS: &str = "id, parent_id, ancestors, handle, title";

fn check_handle(handle: &str) -> Result<()> {
    if is_handle(handle) {
        Ok(())
    } else {
        Err(
            Error::unprocessable("a handle is 1-100 of a-z, 0-9 and single hyphens, e.g. t-shirts")
                .with_code("invalid_handle"),
        )
    }
}

async fn load(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<Category> {
    let id = CategoryId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Category>(&format!(
        "SELECT {COLUMNS} FROM categories WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(Error::not_found)
}

/// A parent named in a request: an unknown one is a 422, not a 404, since
/// the path itself was found.
async fn parent(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<Category> {
    load(tx, tenant, id)
        .await
        .map_err(|_| Error::unprocessable(format!("no category {id}")).with_code("unknown_parent"))
}

fn too_deep() -> Error {
    Error::unprocessable(format!("categories nest at most {MAX_DEPTH} deep")).with_code("too_deep")
}

/// Every category of the shop, 200 per page by default (at most 1000),
/// roots first. Build the tree from `parent_id`. `?parent_id=` lists one
/// level; `?q=` searches title and handle.
#[get("/api/categories", tag = "categories")]
async fn list_categories(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Category>>> {
    let mut tx = tenant.begin_for(&db, &CATEGORIES_READ).await?;
    let page = TenantListing::new("categories", COLUMNS)
        .sortable(["title", "handle"])
        .default_sort("id")
        .search(["title", "handle"])
        .filter_text("parent_id")
        .page_size(200, 1000)
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a category, under `parent_id` or as a root
/// (`categories:write`). Publishes `category.created`.
#[post("/api/categories", tag = "categories", status = 201)]
async fn create_category(
    db: Db,
    tenant: Tenant,
    Valid(Json(body)): Valid<Json<NewCategory>>,
) -> Result<(StatusCode, Json<Category>)> {
    let handle = body.handle.unwrap_or_else(|| handleize(&body.title));
    check_handle(&handle)?;
    let mut tx = tenant.begin_for(&db, &CATEGORIES_WRITE).await?;
    let id = CategoryId::new().to_string();
    let mut ancestors = match &body.parent_id {
        Some(p) => parent(&mut tx, &tenant, p).await?.ancestors,
        None => Vec::new(),
    };
    ancestors.push(id.clone());
    if ancestors.len() > MAX_DEPTH {
        return Err(too_deep());
    }
    let category = sqlx::query_as::<_, Category>(&format!(
        "INSERT INTO categories (id, tenant_id, parent_id, ancestors, handle, title) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
    ))
    .bind(&id)
    .bind(tenant.id())
    .bind(&body.parent_id)
    .bind(&ancestors)
    .bind(&handle)
    .bind(&body.title)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "category.created", &category)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(category)))
}

/// One category (`categories:read`).
#[get("/api/categories/{id}", tag = "categories")]
async fn get_category(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Category>> {
    let mut tx = tenant.begin_for(&db, &CATEGORIES_READ).await?;
    let category = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(category))
}

/// Rename or move a category with its subtree (`categories:write`). Moving
/// it under itself or its own subtree is a 422 `cycle`. Publishes
/// `category.updated`.
#[patch("/api/categories/{id}", tag = "categories")]
async fn update_category(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(change)): Valid<Json<CategoryChange>>,
) -> Result<Json<Category>> {
    if let Some(handle) = &change.handle {
        check_handle(handle)?;
    }
    let mut tx = tenant.begin_for(&db, &CATEGORIES_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    let new_parent = if change.make_root {
        Some(None)
    } else if let Some(p) = &change.parent_id {
        let p = parent(&mut tx, &tenant, p).await?;
        if p.ancestors.contains(&current.id) {
            return Err(
                Error::unprocessable("a category cannot move into its own subtree")
                    .with_code("cycle"),
            );
        }
        Some(Some(p))
    } else {
        None
    };
    if let Some(new_parent) = new_parent {
        let mut prefix = new_parent
            .as_ref()
            .map(|p| p.ancestors.clone())
            .unwrap_or_default();
        prefix.push(current.id.clone());
        // Depth of the deepest category below, counted from `current`.
        let (below,): (Option<i32>,) = sqlx::query_as(
            "SELECT MAX(cardinality(ancestors) - array_position(ancestors, $2)) \
             FROM categories WHERE tenant_id = $1 AND $2 = ANY(ancestors)",
        )
        .bind(tenant.id())
        .bind(&current.id)
        .fetch_one(&mut *tx)
        .await?;
        if prefix.len() + below.unwrap_or(0) as usize > MAX_DEPTH {
            return Err(too_deep());
        }
        // Swap the old path down to `current` for the new one, in the
        // whole subtree.
        sqlx::query(
            "UPDATE categories SET \
             ancestors = $3 || ancestors[array_position(ancestors, $2) + 1:] \
             WHERE tenant_id = $1 AND $2 = ANY(ancestors)",
        )
        .bind(tenant.id())
        .bind(&current.id)
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE categories SET parent_id = $3 WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(&current.id)
            .bind(new_parent.map(|p| p.id))
            .execute(&mut *tx)
            .await?;
    }
    let category = sqlx::query_as::<_, Category>(&format!(
        "UPDATE categories SET handle = COALESCE($3, handle), title = COALESCE($4, title) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(&change.handle)
    .bind(&change.title)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "category.updated", &category)
        .await?;
    tx.commit().await?;
    Ok(Json(category))
}

/// Delete a category and its whole subtree; products stay
/// (`categories:write`). Publishes `category.deleted` with every removed id.
#[delete("/api/categories/{id}", tag = "categories", status = 204)]
async fn delete_category(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CATEGORIES_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    let removed: Vec<(String,)> = sqlx::query_as(
        "DELETE FROM categories WHERE tenant_id = $1 AND $2 = ANY(ancestors) RETURNING id",
    )
    .bind(tenant.id())
    .bind(&current.id)
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<String> = removed.into_iter().map(|(id,)| id).collect();
    tenant
        .publish(
            &mut tx,
            "category.deleted",
            &serde_json::json!({ "id": current.id, "removed": ids }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Products in the category or anywhere below it, 50 per page
/// (`categories:read` and `products:read`). Same `sort`, `q`, `currency`
/// and `tag` parameters as `GET /api/products`.
#[get("/api/categories/{id}/products", tag = "categories")]
async fn list_category_products(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    params: ListParams,
) -> Result<Json<Paged<Product>>> {
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&CATEGORIES_READ)?;
    access.require(&PRODUCTS_READ)?;
    let category = load(&mut tx, &tenant, &id).await?;
    let page = TenantListing::new("products", crate::COLUMNS)
        .scope_subquery(
            "id IN (SELECT pc.product_id FROM product_categories pc \
             JOIN categories c ON c.id = pc.category_id WHERE c.ancestors && ",
            vec![category.id],
            "::text[])",
        )
        .sortable(["sku", "title", "price_minor"])
        .search(["sku", "title"])
        .filter_text("currency")
        .filter_array("tag", "tags")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn product_id(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<String> {
    let id = ProductId::parse(id).map_err(|_| Error::not_found())?;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT id FROM products WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(id,)| id).ok_or_else(Error::not_found)
}

async fn categories_of(tx: &mut TenantTx, tenant: &Tenant, product: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT category_id FROM product_categories \
         WHERE tenant_id = $1 AND product_id = $2 ORDER BY category_id",
    )
    .bind(tenant.id())
    .bind(product)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// The categories a product is directly in (`products:read`).
#[get("/api/products/{id}/categories", tag = "categories")]
async fn get_product_categories(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<ProductCategories>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let product = product_id(&mut tx, &tenant, &id).await?;
    let category_ids = categories_of(&mut tx, &tenant, &product).await?;
    tx.commit().await?;
    Ok(Json(ProductCategories { category_ids }))
}

/// Put a product in exactly these categories (`products:write`). An
/// unknown category is a 422. Publishes `product.categories_changed`.
#[put("/api/products/{id}/categories", tag = "categories")]
async fn set_product_categories(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(body): Json<ProductCategories>,
) -> Result<Json<ProductCategories>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let product = product_id(&mut tx, &tenant, &id).await?;
    let mut wanted = body.category_ids;
    wanted.sort();
    wanted.dedup();
    let (known,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM categories WHERE tenant_id = $1 AND id = ANY($2)")
            .bind(tenant.id())
            .bind(&wanted)
            .fetch_one(&mut *tx)
            .await?;
    if known != wanted.len() as i64 {
        return Err(Error::unprocessable("unknown category id").with_code("unknown_category"));
    }
    sqlx::query("DELETE FROM product_categories WHERE tenant_id = $1 AND product_id = $2")
        .bind(tenant.id())
        .bind(&product)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO product_categories (tenant_id, product_id, category_id) \
         SELECT $1, $2, unnest($3::text[])",
    )
    .bind(tenant.id())
    .bind(&product)
    .bind(&wanted)
    .execute(&mut *tx)
    .await?;
    let result = ProductCategories {
        category_ids: wanted,
    };
    tenant
        .publish(
            &mut tx,
            "product.categories_changed",
            &serde_json::json!({ "product_id": product, "category_ids": result.category_ids }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(result))
}
