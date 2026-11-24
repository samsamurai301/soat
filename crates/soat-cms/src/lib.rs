//! Content: pages built from blocks, menus, and the [`blog`].
//!
//! A page has a handle (its address on the shop, `/pages/<handle>`), a
//! title, SEO fields and a list of blocks (headings, text, images,
//! buttons, product grids, video, spacers, columns; see
//! [`soat_core::content`]). It is a draft until published; the storefront
//! only shows published pages. A menu (`main`, `footer`) is a tree of
//! links, three levels deep. Themes render both. [`redirects`] send old
//! addresses on.

use serde_json::Value;
use soat_core::content::{check_blocks, check_menu};
use soat_core::handle::{handleize, is_handle};
use soat_core::{id_kind, Permission};
use soat_module::Module;
use soat_tenancy::{Scope, Tenant, TenantListing};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;

pub mod blog;
pub mod redirects;
pub mod types;

pub use redirects::redirect_for;

id_kind!(PageId, PageKind, "page");
id_kind!(MenuId, MenuKind, "menu");

/// See pages, blog posts, menus and redirects, drafts included.
pub const CONTENT_READ: Permission = Permission::new(
    "content:read",
    "See pages, blog posts, menus and redirects, drafts included",
)
.for_staff();
/// Create, change, publish and delete pages, blog posts, menus and
/// redirects.
pub const CONTENT_WRITE: Permission = Permission::new(
    "content:write",
    "Create, change, publish and delete pages, blog posts, menus and redirects",
)
.for_staff();

/// The content module.
pub struct Cms;

impl Module for Cms {
    fn name(&self) -> &'static str {
        "cms"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[CONTENT_READ, CONTENT_WRITE, types::CONTENT_TYPES_MANAGE]
    }

    fn subscribers(&self) -> Vec<std::sync::Arc<dyn soat_module::Subscriber>> {
        vec![std::sync::Arc::new(redirects::FollowHandles)]
    }

    fn mount(&self, app: App) -> App {
        let app = types::mount(blog::mount(redirects::mount(app)));
        app.mount((
            list_pages,
            create_page,
            get_page,
            update_page,
            delete_page,
            list_menus,
            create_menu,
            get_menu,
            update_menu,
            delete_menu,
        ))
    }
}

/// A content page.
#[model]
pub struct Page {
    /// e.g. `page_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Its address on the shop: `/pages/<handle>`.
    pub handle: String,
    pub title: String,
    /// What the page shows, top to bottom.
    pub blocks: Vec<Value>,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    /// Whether the storefront shows it.
    pub published: bool,
    /// RFC 3339, UTC; null for a draft.
    pub published_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(sqlx::FromRow)]
struct PageRow {
    id: String,
    handle: String,
    title: String,
    blocks: SqlJson<Vec<Value>>,
    seo_title: Option<String>,
    seo_description: Option<String>,
    published_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<PageRow> for Page {
    fn from(r: PageRow) -> Self {
        Self {
            id: r.id,
            handle: r.handle,
            title: r.title,
            blocks: r.blocks.0,
            seo_title: r.seo_title,
            seo_description: r.seo_description,
            published: r.published_at.is_some(),
            published_at: r.published_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

const PAGE_COLUMNS: &str = "id, handle, title, blocks, seo_title, seo_description, \
     to_char(published_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS published_at, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

/// A page to create, or what replaces one.
#[model]
pub struct PageInput {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    /// Defaults to one made from the title.
    pub handle: Option<String>,
    #[serde(default)]
    pub blocks: Vec<Value>,
    #[validate(length(max = 200))]
    pub seo_title: Option<String>,
    #[validate(length(max = 400))]
    pub seo_description: Option<String>,
    /// Show it on the storefront (default false: a draft).
    #[serde(default)]
    pub published: bool,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::unprocessable(message.into()).with_code("invalid_content")
}

fn handle_of(given: Option<&str>, title: &str) -> Result<String> {
    match given {
        Some(h) if is_handle(h) => Ok(h.to_owned()),
        Some(h) => Err(invalid(format!(
            "{h} is not a handle: lowercase letters, digits and single dashes"
        ))),
        None => Ok(handleize(title)),
    }
}

fn taken(e: sqlx::Error, what: &str) -> Error {
    match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            Error::conflict(format!("the shop already has a {what} with this handle"))
                .with_code("handle_taken")
        }
        _ => e.into(),
    }
}

async fn page(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Page> {
    let id = PageId::parse(id).map_err(|_| Error::not_found())?;
    let row = sqlx::query_as::<_, PageRow>(&format!(
        "SELECT {PAGE_COLUMNS} FROM pages WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)?;
    Ok(row.into())
}

async fn write_page(
    conn: &mut PgConnection,
    tenant: &Tenant,
    id: &str,
    p: &PageInput,
    insert: bool,
) -> Result<Option<Page>> {
    let handle = handle_of(p.handle.as_deref(), &p.title)?;
    check_blocks(&p.blocks).map_err(invalid)?;
    let sql = if insert {
        "INSERT INTO pages (id, tenant_id, handle, title, blocks, seo_title, seo_description, \
         published_at) VALUES ($2, $1, $3, $4, $5, $6, $7, CASE WHEN $8 THEN now() END) \
         RETURNING id"
    } else {
        "UPDATE pages SET handle = $3, title = $4, blocks = $5, seo_title = $6, \
         seo_description = $7, \
         published_at = CASE WHEN $8 THEN COALESCE(published_at, now()) END, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 RETURNING id"
    };
    let blank = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let done: Option<(String,)> = sqlx::query_as(sql)
        .bind(tenant.id())
        .bind(id)
        .bind(&handle)
        .bind(p.title.trim())
        .bind(SqlJson(&p.blocks))
        .bind(blank(&p.seo_title))
        .bind(blank(&p.seo_description))
        .bind(p.published)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| taken(e, "page"))?;
    match done {
        Some((id,)) => Ok(Some(page(conn, tenant, &id).await?)),
        None => Ok(None),
    }
}

/// The shop's pages, newest first (`content:read`). `?q=` searches titles
/// and handles.
#[get("/api/pages", tag = "content")]
async fn list_pages(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Page>>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let page: Paged<PageRow> = TenantListing::new("pages", PAGE_COLUMNS)
        .sortable(["id", "title", "handle"])
        .default_sort("-id")
        .search(["title", "handle"])
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(Paged {
        items: page.items.into_iter().map(Page::from).collect(),
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// Create a page (`content:write`). A handle the shop already uses is a
/// 409 (`handle_taken`); a bad block is a 422 naming it. Publishes
/// `page.created`.
#[post("/api/pages", tag = "content", status = 201)]
async fn create_page(
    db: Db,
    tenant: Tenant,
    Valid(Json(p)): Valid<Json<PageInput>>,
) -> Result<(StatusCode, Json<Page>)> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let made = write_page(&mut tx, &tenant, PageId::new().as_str(), &p, true)
        .await?
        .ok_or_else(|| Error::internal("insert returned nothing"))?;
    tenant.publish(&mut tx, "page.created", &made).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// One page (`content:read`).
#[get("/api/pages/{id}", tag = "content")]
async fn get_page(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Page>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let p = page(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Replace a page; `published: true` publishes it, `false` takes it back to
/// a draft (`content:write`). Publishes `page.updated`.
#[put("/api/pages/{id}", tag = "content")]
async fn update_page(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(p)): Valid<Json<PageInput>>,
) -> Result<Json<Page>> {
    let id = PageId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let p = write_page(&mut tx, &tenant, id.as_str(), &p, false)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "page.updated", &p).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Delete a page (`content:write`). Publishes `page.deleted`.
#[delete("/api/pages/{id}", tag = "content", status = 204)]
async fn delete_page(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let p = page(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM pages WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&p.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "page.deleted",
            &serde_json::json!({ "id": p.id, "handle": p.handle }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A navigation menu.
#[model]
pub struct Menu {
    /// e.g. `menu_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// How themes find it, e.g. `main` or `footer`.
    pub handle: String,
    pub title: String,
    /// Links: `{ "title", "url", "items": [...] }`, three levels deep.
    pub items: Vec<Value>,
    pub updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MenuRow {
    id: String,
    handle: String,
    title: String,
    items: SqlJson<Vec<Value>>,
    updated_at: String,
}

impl From<MenuRow> for Menu {
    fn from(r: MenuRow) -> Self {
        Self {
            id: r.id,
            handle: r.handle,
            title: r.title,
            items: r.items.0,
            updated_at: r.updated_at,
        }
    }
}

const MENU_COLUMNS: &str = "id, handle, title, items, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

/// A menu to create, or what replaces one.
#[model]
pub struct MenuInput {
    #[validate(length(min = 1, max = 100))]
    pub title: String,
    /// Defaults to one made from the title.
    pub handle: Option<String>,
    #[serde(default)]
    pub items: Vec<Value>,
}

async fn menu(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Menu> {
    let id = MenuId::parse(id).map_err(|_| Error::not_found())?;
    let row = sqlx::query_as::<_, MenuRow>(&format!(
        "SELECT {MENU_COLUMNS} FROM menus WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)?;
    Ok(row.into())
}

async fn write_menu(
    conn: &mut PgConnection,
    tenant: &Tenant,
    id: &str,
    m: &MenuInput,
    insert: bool,
) -> Result<Option<Menu>> {
    let handle = handle_of(m.handle.as_deref(), &m.title)?;
    check_menu(&m.items).map_err(invalid)?;
    let sql = if insert {
        "INSERT INTO menus (id, tenant_id, handle, title, items) VALUES ($2, $1, $3, $4, $5) \
         RETURNING id"
    } else {
        "UPDATE menus SET handle = $3, title = $4, items = $5, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 RETURNING id"
    };
    let done: Option<(String,)> = sqlx::query_as(sql)
        .bind(tenant.id())
        .bind(id)
        .bind(&handle)
        .bind(m.title.trim())
        .bind(SqlJson(&m.items))
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| taken(e, "menu"))?;
    match done {
        Some((id,)) => Ok(Some(menu(conn, tenant, &id).await?)),
        None => Ok(None),
    }
}

/// The shop's menus (`content:read`).
#[get("/api/menus", tag = "content")]
async fn list_menus(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Menu>>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let page: Paged<MenuRow> = TenantListing::new("menus", MENU_COLUMNS)
        .sortable(["id", "handle"])
        .default_sort("handle")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(Paged {
        items: page.items.into_iter().map(Menu::from).collect(),
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// Create a menu (`content:write`). Publishes `menu.created`.
#[post("/api/menus", tag = "content", status = 201)]
async fn create_menu(
    db: Db,
    tenant: Tenant,
    Valid(Json(m)): Valid<Json<MenuInput>>,
) -> Result<(StatusCode, Json<Menu>)> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let made = write_menu(&mut tx, &tenant, MenuId::new().as_str(), &m, true)
        .await?
        .ok_or_else(|| Error::internal("insert returned nothing"))?;
    tenant.publish(&mut tx, "menu.created", &made).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// One menu (`content:read`).
#[get("/api/menus/{id}", tag = "content")]
async fn get_menu(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Menu>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let m = menu(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Replace a menu (`content:write`). Publishes `menu.updated`.
#[put("/api/menus/{id}", tag = "content")]
async fn update_menu(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(m)): Valid<Json<MenuInput>>,
) -> Result<Json<Menu>> {
    let id = MenuId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let m = write_menu(&mut tx, &tenant, id.as_str(), &m, false)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "menu.updated", &m).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Delete a menu (`content:write`). Publishes `menu.deleted`.
#[delete("/api/menus/{id}", tag = "content", status = 204)]
async fn delete_menu(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let m = menu(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM menus WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&m.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "menu.deleted",
            &serde_json::json!({ "id": m.id, "handle": m.handle }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A published page by handle, for themes.
pub async fn published_page(
    conn: &mut PgConnection,
    scope: &impl Scope,
    handle: &str,
) -> Result<Option<Page>> {
    if !is_handle(handle) {
        return Ok(None);
    }
    let row = sqlx::query_as::<_, PageRow>(&format!(
        "SELECT {PAGE_COLUMNS} FROM pages WHERE tenant_id = $1 AND handle = $2 \
         AND published_at IS NOT NULL"
    ))
    .bind(scope.tenant_id())
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(Page::from))
}

/// The handles of the shop's published pages, for a sitemap (at most
/// 10 000).
pub async fn published_handles(conn: &mut PgConnection, scope: &impl Scope) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT handle FROM pages WHERE tenant_id = $1 AND published_at IS NOT NULL \
         ORDER BY handle LIMIT 10000",
    )
    .bind(scope.tenant_id())
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(h,)| h).collect())
}

/// A menu by handle, for themes.
pub async fn menu_by_handle(
    conn: &mut PgConnection,
    scope: &impl Scope,
    handle: &str,
) -> Result<Option<Menu>> {
    if !is_handle(handle) {
        return Ok(None);
    }
    let row = sqlx::query_as::<_, MenuRow>(&format!(
        "SELECT {MENU_COLUMNS} FROM menus WHERE tenant_id = $1 AND handle = $2"
    ))
    .bind(scope.tenant_id())
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(Menu::from))
}
