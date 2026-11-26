//! URL redirects: an old address on the shop that sends buyers and search
//! engines on with a `301`. Staff add them; moving a product or collection
//! to a new handle adds one by itself, and points older redirects at the
//! new address so there are no chains.

use soat_core::content::is_link;
use soat_core::id_kind;
use soat_module::{Event, HandlerFuture, Subscriber};
use soat_tenancy::{EventShop, Scope, Tenant, TenantListing};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;
use zapline::Resources;

use crate::{CONTENT_READ, CONTENT_WRITE};

id_kind!(RedirectId, RedirectKind, "redir");

/// Longest path or address of a redirect.
pub const MAX_PATH: usize = 500;

/// A redirect.
#[model]
#[derive(sqlx::FromRow)]
pub struct Redirect {
    /// e.g. `redir_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// The old path on the shop, e.g. `/products/old-name`.
    pub from_path: String,
    /// Where it sends buyers: a path on the shop or an `https://` address.
    pub to_url: String,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// Body of a new redirect.
#[model]
pub struct NewRedirect {
    /// A path on the shop, starting with `/`.
    pub from_path: String,
    /// A path on the shop or an `http(s)://` address.
    pub to_url: String,
}

const COLUMNS: &str = "id, from_path, to_url, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

pub(crate) fn mount(app: App) -> App {
    app.mount((list_redirects, create_redirect, delete_redirect))
}

/// `path` as stored: no trailing slash (but for `/`), no fragment. `None`
/// when it is not a path on the shop.
pub fn normalize(path: &str) -> Option<String> {
    let path = path.trim();
    let path = path.split('#').next().unwrap_or_default();
    let ok = path.starts_with('/')
        && !path.starts_with("//")
        && path.len() <= MAX_PATH
        && !path
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\');
    if !ok {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    Some(if trimmed.is_empty() { "/" } else { trimmed }.to_owned())
}

/// Where the shop sends `path`, if anywhere. Run it in the shop's
/// transaction.
///
/// # Errors
///
/// Database errors.
pub async fn redirect_for(
    conn: &mut PgConnection,
    scope: &impl Scope,
    path: &str,
) -> Result<Option<String>> {
    let Some(path) = normalize(path) else {
        return Ok(None);
    };
    let row: Option<(String,)> =
        sqlx::query_as("SELECT to_url FROM url_redirects WHERE tenant_id = $1 AND from_path = $2")
            .bind(scope.tenant_id())
            .bind(&path)
            .fetch_optional(conn)
            .await?;
    Ok(row.map(|(to,)| to))
}

/// The shop's redirects, newest first (`content:read`). `?q=` searches
/// both paths.
#[get("/api/redirects", tag = "content")]
async fn list_redirects(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Redirect>>> {
    let mut tx = tenant.begin_for(&db, &CONTENT_READ).await?;
    let page = TenantListing::new("url_redirects", COLUMNS)
        .sortable(["id", "from_path"])
        .default_sort("-id")
        .search(["from_path", "to_url"])
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Add a redirect (`content:write`). A path that already redirects is a
/// 409; a bad path or target, or one that points at itself, a 422
/// `invalid_redirect`. Publishes `redirect.created`.
#[post("/api/redirects", tag = "content", status = 201)]
async fn create_redirect(
    db: Db,
    tenant: Tenant,
    Json(r): Json<NewRedirect>,
) -> Result<(StatusCode, Json<Redirect>)> {
    let bad = |m: &str| Error::unprocessable(m.to_owned()).with_code("invalid_redirect");
    let from = normalize(&r.from_path)
        .filter(|p| p != "/")
        .ok_or_else(|| bad("from_path is a path on the shop, e.g. /products/old-name"))?;
    let to = r.to_url.trim();
    if to.len() > MAX_PATH || !is_link(to) {
        return Err(bad("to_url is a path on the shop or an http(s) address"));
    }
    let to = if to.starts_with('/') {
        normalize(to).ok_or_else(|| bad("to_url is not a path"))?
    } else {
        to.to_owned()
    };
    if to == from {
        return Err(bad("a redirect cannot point at itself"));
    }
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let made = sqlx::query_as::<_, Redirect>(&format!(
        "INSERT INTO url_redirects (id, tenant_id, from_path, to_url) VALUES ($1, $2, $3, $4) \
         RETURNING {COLUMNS}"
    ))
    .bind(RedirectId::new().as_str())
    .bind(tenant.id())
    .bind(&from)
    .bind(&to)
    .fetch_one(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "redirect.created", &made).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// Remove a redirect (`content:write`). Publishes `redirect.deleted`.
#[delete("/api/redirects/{id}", tag = "content", status = 204)]
async fn delete_redirect(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = RedirectId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &CONTENT_WRITE).await?;
    let gone = sqlx::query_as::<_, Redirect>(&format!(
        "DELETE FROM url_redirects WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "redirect.deleted", &gone).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Keeps old product and collection links working when their handle
/// changes. Idempotent: running it twice leaves the same redirects.
pub(crate) struct FollowHandles;

impl Subscriber for FollowHandles {
    fn name(&self) -> &'static str {
        "cms-redirects"
    }

    fn wants(&self, kind: &str) -> bool {
        matches!(kind, "product.handle_changed" | "collection.handle_changed")
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let base = if event.kind.starts_with("product.") {
                "/products/"
            } else {
                "/collections/"
            };
            let (Some(from), Some(to)) =
                (event.payload["from"].as_str(), event.payload["to"].as_str())
            else {
                return Ok(());
            };
            let (from, to) = (format!("{base}{from}"), format!("{base}{to}"));
            let pool: PgPool = res.require()?;
            let shop = EventShop(event.tenant_id.clone());
            let mut tx = shop.begin_on(&pool).await?;
            // The new address is a page again; older redirects follow it.
            sqlx::query("DELETE FROM url_redirects WHERE tenant_id = $1 AND from_path = $2")
                .bind(shop.tenant_id())
                .bind(&to)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE url_redirects SET to_url = $3 WHERE tenant_id = $1 AND to_url = $2",
            )
            .bind(shop.tenant_id())
            .bind(&from)
            .bind(&to)
            .execute(&mut *tx)
            .await?;
            let made: Option<Redirect> = sqlx::query_as(&format!(
                "INSERT INTO url_redirects (id, tenant_id, from_path, to_url) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (tenant_id, from_path) DO UPDATE SET to_url = EXCLUDED.to_url \
                 RETURNING {COLUMNS}"
            ))
            .bind(RedirectId::new().as_str())
            .bind(shop.tenant_id())
            .bind(&from)
            .bind(&to)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(r) = made {
                shop.publish(&mut tx, "redirect.created", &r).await?;
            }
            tx.commit().await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(normalize("/a/b/").as_deref(), Some("/a/b"));
        assert_eq!(normalize("/").as_deref(), Some("/"));
        assert_eq!(normalize("/x#top").as_deref(), Some("/x"));
        assert_eq!(normalize("/a?b=1").as_deref(), Some("/a?b=1"));
        for bad in ["a", "//evil.example", "/a b", "/a\\b", ""] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }
}
