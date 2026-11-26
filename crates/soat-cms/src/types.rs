//! Content types the shop defines for itself: a store location, a size
//! chart, an FAQ entry. A type has typed fields (the kinds of custom
//! fields); entries fill them in and are published or drafts. Public
//! types' published entries are on the storefront API, for themes and
//! headless front ends.

use serde_json::{Map, Value};
use soat_core::content_types::{check_entry, check_fields, title_of, ContentField};
use soat_core::fields::is_field_key;
use soat_core::handle::{handleize, is_handle};
use soat_core::Permission;
use soat_tenancy::{Scope, Shop, Tenant, TenantListing};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{invalid, taken, CONTENT_READ, CONTENT_WRITE};

soat_core::id_kind!(EntryId, EntryKind, "entry");

/// Define, change and delete the shop's content types.
pub const CONTENT_TYPES_MANAGE: Permission = Permission::new(
    "content_types:manage",
    "Define, change and delete the shop's content types",
);

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_types,
        get_type,
        put_type,
        delete_type,
        list_entries,
        create_entry,
        get_entry,
        update_entry,
        delete_entry,
    ))
    .mount((storefront_entries, storefront_entry))
}

/// A field of a content type.
#[model]
pub struct TypeField {
    /// e.g. `opening_hours`: a-z, 0-9 and `_`, starting with a letter.
    pub key: String,
    /// Shown to staff, e.g. `Opening hours`.
    pub name: String,
    /// `text`, `integer`, `decimal`, `boolean`, `date`, `url`, `choice`,
    /// `text_list` or `json`.
    pub kind: String,
    /// The allowed values of a `choice` field.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Every entry must have a value.
    #[serde(default)]
    pub required: bool,
}

impl From<ContentField> for TypeField {
    fn from(f: ContentField) -> Self {
        Self {
            key: f.key,
            name: f.name,
            kind: f.kind,
            choices: f.choices,
            required: f.required,
        }
    }
}

impl From<&TypeField> for ContentField {
    fn from(f: &TypeField) -> Self {
        Self {
            key: f.key.clone(),
            name: f.name.clone(),
            kind: f.kind.clone(),
            choices: f.choices.clone(),
            required: f.required,
        }
    }
}

/// A content type.
#[model]
pub struct ContentType {
    /// e.g. `store_location`; entries live at `/api/content/<key>`.
    pub key: String,
    /// e.g. `Store location`.
    pub name: String,
    pub fields: Vec<TypeField>,
    /// Whether published entries are on the storefront API.
    pub public: bool,
    /// How many entries it has.
    pub entries: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `PUT /api/content-types/{key}`.
#[model]
pub struct TypeInput {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    /// 1 to 50 fields.
    pub fields: Vec<TypeField>,
    /// Show published entries on the storefront API (default false).
    #[serde(default)]
    pub public: bool,
}

/// An entry of a content type.
#[model]
pub struct Entry {
    /// e.g. `entry_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// The type's key.
    pub content_type: String,
    /// Unique within the type.
    pub handle: String,
    /// The values, by field key; empty fields are left out.
    pub fields: Map<String, Value>,
    pub published: bool,
    /// RFC 3339, UTC; null for a draft.
    pub published_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// An entry to create, or what replaces one.
#[model]
pub struct EntryInput {
    /// Defaults to one made from the first text field.
    pub handle: Option<String>,
    /// Values by field key; `null` or left out is empty.
    #[serde(default)]
    pub fields: Map<String, Value>,
    /// Publish it (default false: a draft).
    #[serde(default)]
    pub published: bool,
}

const TYPE_COLUMNS: &str = "t.key, t.name, t.fields, t.public, \
     (SELECT count(*) FROM content_entries e WHERE e.tenant_id = t.tenant_id \
      AND e.type_key = t.key) AS entries, \
     to_char(t.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(t.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

const ENTRY_COLUMNS: &str = "id, type_key, handle, fields, published_at IS NOT NULL AS published, \
     to_char(published_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS published_at, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

/// An entry's values by field key.
type Values = Map<String, Value>;

type TypeRow = (
    String,
    String,
    SqlJson<Vec<ContentField>>,
    bool,
    i64,
    String,
    String,
);

fn type_view(row: TypeRow) -> ContentType {
    let (key, name, fields, public, entries, created_at, updated_at) = row;
    ContentType {
        key,
        name,
        fields: fields.0.into_iter().map(TypeField::from).collect(),
        public,
        entries,
        created_at,
        updated_at,
    }
}

#[derive(sqlx::FromRow)]
struct EntryRow {
    id: String,
    type_key: String,
    handle: String,
    fields: SqlJson<Map<String, Value>>,
    published: bool,
    published_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<EntryRow> for Entry {
    fn from(r: EntryRow) -> Self {
        Self {
            id: r.id,
            content_type: r.type_key,
            handle: r.handle,
            fields: r.fields.0,
            published: r.published,
            published_at: r.published_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

fn paged(page: Paged<EntryRow>) -> Paged<Entry> {
    Paged {
        items: page.items.into_iter().map(Entry::from).collect(),
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }
}

/// The content type `key`, if the shop has it.
async fn find_type(
    conn: &mut PgConnection,
    scope: &impl Scope,
    key: &str,
) -> Result<Option<ContentType>> {
    if !is_field_key(key) {
        return Ok(None);
    }
    let row: Option<TypeRow> = sqlx::query_as(&format!(
        "SELECT {TYPE_COLUMNS} FROM content_types t WHERE t.tenant_id = $1 AND t.key = $2"
    ))
    .bind(scope.tenant_id())
    .bind(key)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(type_view))
}

async fn content_type(
    conn: &mut PgConnection,
    scope: &impl Scope,
    key: &str,
) -> Result<ContentType> {
    find_type(conn, scope, key)
        .await?
        .ok_or_else(Error::not_found)
}

async fn entry(
    conn: &mut PgConnection,
    scope: &impl Scope,
    type_key: &str,
    id: &str,
) -> Result<Entry> {
    let id = EntryId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, EntryRow>(&format!(
        "SELECT {ENTRY_COLUMNS} FROM content_entries \
         WHERE tenant_id = $1 AND type_key = $2 AND id = $3"
    ))
    .bind(scope.tenant_id())
    .bind(type_key)
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .map(Entry::from)
    .ok_or_else(Error::not_found)
}

/// The shop's content types, by key (`content:read`).
#[get("/api/content-types", tag = "content")]
async fn list_types(db: Db, tenant: Tenant) -> Result<Json<Vec<ContentType>>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let rows: Vec<TypeRow> = sqlx::query_as(&format!(
        "SELECT {TYPE_COLUMNS} FROM content_types t WHERE t.tenant_id = $1 ORDER BY t.key"
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(rows.into_iter().map(type_view).collect()))
}

/// One content type (`content:read`).
#[get("/api/content-types/{key}", tag = "content")]
async fn get_type(db: Db, tenant: Tenant, Path(key): Path<String>) -> Result<Json<ContentType>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let t = content_type(&mut tx, &tenant, &key).await?;
    tx.commit().await?;
    Ok(Json(t))
}

/// Define a content type, or change its name, fields or visibility
/// (`content_types:manage`). Values of fields taken out are dropped from
/// its entries; a change an entry no longer fits (a new required field, a
/// changed kind) is a 409 `entries_do_not_fit`. Publishes
/// `content_type.defined`.
#[put("/api/content-types/{key}", tag = "content")]
async fn put_type(
    db: Db,
    tenant: Tenant,
    Path(key): Path<String>,
    Valid(Json(body)): Valid<Json<TypeInput>>,
) -> Result<Json<ContentType>> {
    if !is_field_key(&key) {
        return Err(invalid(
            "a content type key is 1-64 of a-z, 0-9 and _, starting with a letter",
        ));
    }
    let given: Vec<ContentField> = body.fields.iter().map(ContentField::from).collect();
    let fields = check_fields(&given).map_err(invalid)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(invalid("name cannot be blank"));
    }
    let mut tx = tenant.begin_for(&db, &CONTENT_TYPES_MANAGE).await?;
    // Every entry must still fit; values of fields taken out are dropped.
    let entries: Vec<(String, String, SqlJson<Values>)> = sqlx::query_as(
        "SELECT id, handle, fields FROM content_entries WHERE tenant_id = $1 AND type_key = $2 \
         FOR UPDATE",
    )
    .bind(tenant.id())
    .bind(&key)
    .fetch_all(&mut *tx)
    .await?;
    for (id, handle, SqlJson(mut values)) in entries {
        values.retain(|k, _| fields.iter().any(|f| &f.key == k));
        let fitted = check_entry(&fields, &values).map_err(|e| {
            Error::conflict(format!("entry {handle} would not fit: {e}"))
                .with_code("entries_do_not_fit")
        })?;
        sqlx::query("UPDATE content_entries SET fields = $3 WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(&id)
            .bind(SqlJson(&fitted))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO content_types (tenant_id, key, name, fields, public) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (tenant_id, key) DO UPDATE SET name = EXCLUDED.name, \
         fields = EXCLUDED.fields, public = EXCLUDED.public, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(&key)
    .bind(name)
    .bind(SqlJson(&fields))
    .bind(body.public)
    .execute(&mut *tx)
    .await?;
    let t = content_type(&mut tx, &tenant, &key).await?;
    tenant.publish(&mut tx, "content_type.defined", &t).await?;
    tx.commit().await?;
    Ok(Json(t))
}

/// Delete a content type without entries (`content_types:manage`); one
/// with entries is a 409 `content_type_in_use`. Publishes
/// `content_type.deleted`.
#[delete("/api/content-types/{key}", tag = "content", status = 204)]
async fn delete_type(db: Db, tenant: Tenant, Path(key): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CONTENT_TYPES_MANAGE).await?;
    let t = content_type(&mut tx, &tenant, &key).await?;
    if t.entries > 0 {
        return Err(Error::conflict(format!(
            "{} entries use this type; delete them first",
            t.entries
        ))
        .with_code("content_type_in_use"));
    }
    sqlx::query("DELETE FROM content_types WHERE tenant_id = $1 AND key = $2")
        .bind(tenant.id())
        .bind(&t.key)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "content_type.deleted",
            &serde_json::json!({ "key": t.key }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The entries of a content type, drafts included, newest first
/// (`content:read`). `?q=` searches handles; sort by `handle` or
/// `updated_at`.
#[get("/api/content/{type}", tag = "content")]
async fn list_entries(
    db: Db,
    tenant: Tenant,
    Path(type_key): Path<String>,
    params: ListParams,
) -> Result<Json<Paged<Entry>>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let t = content_type(&mut tx, &tenant, &type_key).await?;
    let page = TenantListing::new("content_entries", ENTRY_COLUMNS)
        .scope_text("type_key", t.key)
        .sortable(["id", "handle", "updated_at"])
        .default_sort("-id")
        .search(["handle"])
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(paged(page)))
}

async fn write(
    conn: &mut PgConnection,
    tenant: &Tenant,
    t: &ContentType,
    id: &str,
    e: &EntryInput,
    insert: bool,
) -> Result<Option<Entry>> {
    let fields: Vec<ContentField> = t.fields.iter().map(ContentField::from).collect();
    let values = check_entry(&fields, &e.fields)
        .map_err(|m| Error::unprocessable(m).with_code("invalid_entry"))?;
    let handle = match e.handle.as_deref() {
        Some(h) if is_handle(h) => h.to_owned(),
        Some(h) => {
            return Err(invalid(format!(
                "{h} is not a handle: lowercase letters, digits and single dashes"
            )))
        }
        None => title_of(&fields, &values)
            .map(|t| handleize(&t))
            .filter(|h| is_handle(h))
            .unwrap_or_else(|| handleize(id)),
    };
    let sql = if insert {
        "INSERT INTO content_entries (id, tenant_id, type_key, handle, fields, published_at) \
         VALUES ($2, $1, $3, $4, $5, CASE WHEN $6 THEN now() END) RETURNING id"
    } else {
        "UPDATE content_entries SET handle = $4, fields = $5, \
         published_at = CASE WHEN $6 THEN COALESCE(published_at, now()) END, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND type_key = $3 RETURNING id"
    };
    let done: Option<(String,)> = sqlx::query_as(sql)
        .bind(tenant.id())
        .bind(id)
        .bind(&t.key)
        .bind(&handle)
        .bind(SqlJson(&values))
        .bind(e.published)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| taken(e, "entry of this type"))?;
    match done {
        Some((id,)) => Ok(Some(entry(conn, tenant, &t.key, &id).await?)),
        None => Ok(None),
    }
}

/// Add an entry (`content:write`). A value that does not fit its field,
/// or a missing required one, is a 422 `invalid_entry`; a handle the type
/// already has is a 409 `handle_taken`. Publishes `content_entry.created`.
#[post("/api/content/{type}", tag = "content", status = 201)]
async fn create_entry(
    db: Db,
    tenant: Tenant,
    Path(type_key): Path<String>,
    Valid(Json(e)): Valid<Json<EntryInput>>,
) -> Result<(StatusCode, Json<Entry>)> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let t = content_type(&mut tx, &tenant, &type_key).await?;
    let made = write(&mut tx, &tenant, &t, EntryId::new().as_str(), &e, true)
        .await?
        .ok_or_else(|| Error::internal("insert returned nothing"))?;
    tenant
        .publish(&mut tx, "content_entry.created", &made)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// One entry (`content:read`).
#[get("/api/content/{type}/{id}", tag = "content")]
async fn get_entry(
    db: Db,
    tenant: Tenant,
    Path((type_key, id)): Path<(String, String)>,
) -> Result<Json<Entry>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let e = entry(&mut tx, &tenant, &type_key, &id).await?;
    tx.commit().await?;
    Ok(Json(e))
}

/// Replace an entry; `published: false` takes it back to a draft
/// (`content:write`). Publishes `content_entry.updated`.
#[put("/api/content/{type}/{id}", tag = "content")]
async fn update_entry(
    db: Db,
    tenant: Tenant,
    Path((type_key, id)): Path<(String, String)>,
    Valid(Json(e)): Valid<Json<EntryInput>>,
) -> Result<Json<Entry>> {
    let id = EntryId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let t = content_type(&mut tx, &tenant, &type_key).await?;
    let e = write(&mut tx, &tenant, &t, id.as_str(), &e, false)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "content_entry.updated", &e).await?;
    tx.commit().await?;
    Ok(Json(e))
}

/// Delete an entry (`content:write`). Publishes `content_entry.deleted`.
#[delete("/api/content/{type}/{id}", tag = "content", status = 204)]
async fn delete_entry(
    db: Db,
    tenant: Tenant,
    Path((type_key, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let e = entry(&mut tx, &tenant, &type_key, &id).await?;
    sqlx::query("DELETE FROM content_entries WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&e.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "content_entry.deleted",
            &serde_json::json!({ "id": e.id, "content_type": e.content_type, "handle": e.handle }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A public content type's published entries, by handle. Run it in the
/// shop's transaction; `None` when the type is unknown or not public.
pub async fn published_entries(
    conn: &mut PgConnection,
    scope: &impl Scope,
    type_key: &str,
    limit: i64,
    offset: i64,
) -> Result<Option<Paged<Entry>>> {
    if !find_type(conn, scope, type_key)
        .await?
        .is_some_and(|t| t.public)
    {
        return Ok(None);
    }
    let limit = limit.clamp(1, 100);
    let offset = offset.max(0);
    let filter = "tenant_id = $1 AND type_key = $2 AND published_at IS NOT NULL";
    let rows = sqlx::query_as::<_, EntryRow>(&format!(
        "SELECT {ENTRY_COLUMNS} FROM content_entries WHERE {filter} \
         ORDER BY handle LIMIT $3 OFFSET $4"
    ))
    .bind(scope.tenant_id())
    .bind(type_key)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *conn)
    .await?;
    let (total,): (i64,) = sqlx::query_as(&format!(
        "SELECT count(*) FROM content_entries WHERE {filter}"
    ))
    .bind(scope.tenant_id())
    .bind(type_key)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(Paged {
        items: rows.into_iter().map(Entry::from).collect(),
        total,
        limit,
        offset,
    }))
}

/// A public content type's published entry by handle. Run it in the
/// shop's transaction.
pub async fn published_entry(
    conn: &mut PgConnection,
    scope: &impl Scope,
    type_key: &str,
    handle: &str,
) -> Result<Option<Entry>> {
    if !is_handle(handle)
        || !find_type(conn, scope, type_key)
            .await?
            .is_some_and(|t| t.public)
    {
        return Ok(None);
    }
    Ok(sqlx::query_as::<_, EntryRow>(&format!(
        "SELECT {ENTRY_COLUMNS} FROM content_entries WHERE tenant_id = $1 AND type_key = $2 \
         AND handle = $3 AND published_at IS NOT NULL"
    ))
    .bind(scope.tenant_id())
    .bind(type_key)
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?
    .map(Entry::from))
}

fn number(params: &ListParams, name: &str, default: i64) -> i64 {
    params
        .get(name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A public content type's published entries by handle (no sign-in);
/// `?limit=` (at most 100, default 50) and `?offset=`. A type that is not
/// public is a 404.
#[get("/api/storefront/content/{type}", tag = "storefront")]
async fn storefront_entries(
    db: Db,
    shop: Shop,
    Path(type_key): Path<String>,
    params: ListParams,
) -> Result<Json<Paged<Entry>>> {
    let mut tx = shop.begin(&db).await?;
    let page = published_entries(
        &mut tx,
        &shop,
        &type_key,
        number(&params, "limit", 50),
        number(&params, "offset", 0),
    )
    .await?;
    tx.commit().await?;
    page.map(Json).ok_or_else(Error::not_found)
}

/// A published entry of a public content type, by handle (no sign-in).
#[get("/api/storefront/content/{type}/{handle}", tag = "storefront")]
async fn storefront_entry(
    db: Db,
    shop: Shop,
    Path((type_key, handle)): Path<(String, String)>,
) -> Result<Json<Entry>> {
    let mut tx = shop.begin(&db).await?;
    let e = published_entry(&mut tx, &shop, &type_key, &handle).await?;
    tx.commit().await?;
    e.map(Json).ok_or_else(Error::not_found)
}
