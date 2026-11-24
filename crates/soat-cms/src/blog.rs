//! The blog: posts in Markdown with an excerpt, an image, an author and
//! tags, published now or at a set time. The storefront shows published
//! posts newest first, by tag, with an Atom feed.

use soat_core::handle::is_handle;
use soat_tenancy::{Scope, Shop, Tenant, TenantListing};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{handle_of, invalid, taken, CONTENT_READ, CONTENT_WRITE};

soat_core::id_kind!(PostId, PostKind, "post");

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_posts,
        create_post,
        get_post,
        update_post,
        delete_post,
        storefront_posts,
        storefront_post,
    ))
}

/// A blog post.
#[model]
#[derive(sqlx::FromRow)]
pub struct Post {
    /// e.g. `post_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Its address on the shop: `/blog/<handle>`.
    pub handle: String,
    pub title: String,
    /// A line or two for lists and feeds.
    pub excerpt: String,
    /// Markdown.
    pub body: String,
    /// An image above the post and in lists.
    pub image_url: Option<String>,
    pub author: String,
    pub tags: Vec<String>,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    /// Whether it is published; the storefront shows it from
    /// `published_at` on.
    pub published: bool,
    /// RFC 3339, UTC; in the future for a scheduled post, null for a
    /// draft.
    pub published_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "id, handle, title, excerpt, body, image_url, author, tags, seo_title, \
     seo_description, published_at IS NOT NULL AS published, \
     to_char(published_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS published_at, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

/// A post to create, or what replaces one.
#[model]
pub struct PostInput {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    /// Defaults to one made from the title.
    pub handle: Option<String>,
    #[validate(length(max = 1000))]
    pub excerpt: Option<String>,
    /// Markdown.
    #[serde(default)]
    #[validate(length(max = 200_000))]
    pub body: String,
    #[validate(url, length(max = 2000))]
    pub image_url: Option<String>,
    #[validate(length(max = 200))]
    pub author: Option<String>,
    /// At most 20, each at most 50 characters.
    #[serde(default)]
    pub tags: Vec<String>,
    #[validate(length(max = 200))]
    pub seo_title: Option<String>,
    #[validate(length(max = 400))]
    pub seo_description: Option<String>,
    /// Publish it (default false: a draft).
    #[serde(default)]
    pub published: bool,
    /// When it shows, RFC 3339; a time ahead schedules it. Defaults to
    /// now, or the time it was first published.
    pub published_at: Option<String>,
}

fn blank(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Tags trimmed, without repeats, in the order given.
fn tags(given: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in given.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if t.chars().count() > 50 {
            return Err(invalid(format!("the tag {t} is longer than 50 characters")));
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(t)) {
            out.push(t.to_owned());
        }
    }
    if out.len() > 20 {
        return Err(invalid("a post has at most 20 tags"));
    }
    Ok(out)
}

async fn post(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Post> {
    let id = PostId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Post>(&format!(
        "SELECT {COLUMNS} FROM blog_posts WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)
}

async fn write(
    conn: &mut PgConnection,
    tenant: &Tenant,
    id: &str,
    p: &PostInput,
    insert: bool,
) -> Result<Option<Post>> {
    let handle = handle_of(p.handle.as_deref(), &p.title)?;
    let tags = tags(&p.tags)?;
    let at = match blank(&p.published_at) {
        Some(v) if v.len() > 40 || !v.contains('T') => {
            return Err(invalid(format!("{v} is not an RFC 3339 time")))
        }
        other => other,
    };
    let sql = if insert {
        "INSERT INTO blog_posts (id, tenant_id, handle, title, excerpt, body, image_url, author, \
         tags, seo_title, seo_description, published_at) \
         VALUES ($2, $1, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
         CASE WHEN $12 THEN COALESCE($13::timestamptz, now()) END) RETURNING id"
    } else {
        "UPDATE blog_posts SET handle = $3, title = $4, excerpt = $5, body = $6, image_url = $7, \
         author = $8, tags = $9, seo_title = $10, seo_description = $11, \
         published_at = CASE WHEN $12 THEN COALESCE($13::timestamptz, published_at, now()) END, \
         updated_at = now() WHERE tenant_id = $1 AND id = $2 RETURNING id"
    };
    let done: Option<(String,)> = sqlx::query_as(sql)
        .bind(tenant.id())
        .bind(id)
        .bind(&handle)
        .bind(p.title.trim())
        .bind(p.excerpt.as_deref().unwrap_or("").trim())
        .bind(&p.body)
        .bind(blank(&p.image_url))
        .bind(p.author.as_deref().unwrap_or("").trim())
        .bind(&tags)
        .bind(blank(&p.seo_title))
        .bind(blank(&p.seo_description))
        .bind(p.published)
        .bind(at)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("22007") => {
                invalid("published_at is an RFC 3339 time")
            }
            _ => taken(e, "post"),
        })?;
    match done {
        Some((id,)) => Ok(Some(post(conn, tenant, &id).await?)),
        None => Ok(None),
    }
}

/// The shop's blog posts, drafts and scheduled ones included, newest
/// first (`content:read`). `?q=` searches titles and handles; filter with
/// `?tag=`.
#[get("/api/blog/posts", tag = "content")]
async fn list_posts(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Post>>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let page = TenantListing::new("blog_posts", COLUMNS)
        .sortable(["id", "title", "published_at"])
        .default_sort("-id")
        .search(["title", "handle"])
        .filter_array("tag", "tags")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Write a blog post (`content:write`). A handle the shop already uses is
/// a 409 (`handle_taken`). Publishes `blog_post.created`.
#[post("/api/blog/posts", tag = "content", status = 201)]
async fn create_post(
    db: Db,
    tenant: Tenant,
    Valid(Json(p)): Valid<Json<PostInput>>,
) -> Result<(StatusCode, Json<Post>)> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let made = write(&mut tx, &tenant, PostId::new().as_str(), &p, true)
        .await?
        .ok_or_else(|| Error::internal("insert returned nothing"))?;
    tenant.publish(&mut tx, "blog_post.created", &made).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// One blog post (`content:read`).
#[get("/api/blog/posts/{id}", tag = "content")]
async fn get_post(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Post>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let p = post(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Replace a blog post; `published: false` takes it back to a draft
/// (`content:write`). Publishes `blog_post.updated`.
#[put("/api/blog/posts/{id}", tag = "content")]
async fn update_post(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(p)): Valid<Json<PostInput>>,
) -> Result<Json<Post>> {
    let id = PostId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let p = write(&mut tx, &tenant, id.as_str(), &p, false)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "blog_post.updated", &p).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Delete a blog post (`content:write`). Publishes `blog_post.deleted`.
#[delete("/api/blog/posts/{id}", tag = "content", status = 204)]
async fn delete_post(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let p = post(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM blog_posts WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&p.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "blog_post.deleted",
            &serde_json::json!({ "id": p.id, "handle": p.handle }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Published posts, newest first, optionally with `tag`; and how many
/// there are in all. Run it in the shop's transaction.
pub async fn published_posts(
    conn: &mut PgConnection,
    scope: &impl Scope,
    tag: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<Post>, i64)> {
    let tag = tag.map(str::trim).filter(|t| !t.is_empty());
    let filter = "tenant_id = $1 AND published_at <= now() \
         AND ($2::text IS NULL OR EXISTS (SELECT 1 FROM unnest(tags) t WHERE lower(t) = lower($2)))";
    let posts = sqlx::query_as::<_, Post>(&format!(
        "SELECT {COLUMNS} FROM blog_posts WHERE {filter} \
         ORDER BY published_at DESC, id DESC LIMIT $3 OFFSET $4"
    ))
    .bind(scope.tenant_id())
    .bind(tag)
    .bind(limit.clamp(1, 100))
    .bind(offset.max(0))
    .fetch_all(&mut *conn)
    .await?;
    let total: (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM blog_posts WHERE {filter}"))
        .bind(scope.tenant_id())
        .bind(tag)
        .fetch_one(&mut *conn)
        .await?;
    Ok((posts, total.0))
}

/// A published post by handle. Run it in the shop's transaction.
pub async fn published_post(
    conn: &mut PgConnection,
    scope: &impl Scope,
    handle: &str,
) -> Result<Option<Post>> {
    if !is_handle(handle) {
        return Ok(None);
    }
    Ok(sqlx::query_as::<_, Post>(&format!(
        "SELECT {COLUMNS} FROM blog_posts WHERE tenant_id = $1 AND handle = $2 \
         AND published_at <= now()"
    ))
    .bind(scope.tenant_id())
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?)
}

/// The handles of published posts, for a sitemap (at most 10 000).
pub async fn published_post_handles(
    conn: &mut PgConnection,
    scope: &impl Scope,
) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT handle FROM blog_posts WHERE tenant_id = $1 AND published_at <= now() \
         ORDER BY published_at DESC LIMIT 10000",
    )
    .bind(scope.tenant_id())
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(h,)| h).collect())
}

/// Query of the storefront's blog list.
#[model]
pub struct BlogQuery {
    pub tag: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// The shop's published blog posts, newest first; `?tag=` for one tag.
#[get("/api/storefront/blog", tag = "storefront")]
async fn storefront_posts(
    db: Db,
    shop: Shop,
    Query(q): Query<BlogQuery>,
) -> Result<Json<Paged<Post>>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);
    let mut tx = shop.begin(&db).await?;
    let (items, total) = published_posts(&mut tx, &shop, q.tag.as_deref(), limit, offset).await?;
    tx.commit().await?;
    Ok(Json(Paged {
        items,
        total,
        limit,
        offset,
    }))
}

/// A published blog post by handle.
#[get("/api/storefront/blog/{handle}", tag = "storefront")]
async fn storefront_post(db: Db, shop: Shop, Path(handle): Path<String>) -> Result<Json<Post>> {
    let mut tx = shop.begin(&db).await?;
    let p = published_post(&mut tx, &shop, &handle).await?;
    tx.commit().await?;
    p.map(Json).ok_or_else(Error::not_found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_tidied() {
        let t = tags(&[" News ".into(), "news".into(), "".into(), "Recipes".into()]).unwrap();
        assert_eq!(t, ["News", "Recipes"]);
        assert!(tags(&["x".repeat(51)]).is_err());
        assert!(tags(&(0..21).map(|i| i.to_string()).collect::<Vec<_>>()).is_err());
    }
}
