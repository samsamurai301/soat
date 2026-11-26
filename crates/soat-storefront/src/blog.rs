//! The blog: `/blog` (newest first, `?tag=` and `?offset=`),
//! `/blog/{handle}` and an Atom feed at `/blog/feed.xml`.

use serde_json::{json, Value};
use soat_cms::blog::{published_post, published_posts, Post};
use soat_tenancy::Scope;
use zapline::db::{sqlx, Db, ListParams};
use zapline::prelude::*;

use crate::seo::{text, xml_escape};
use crate::{encode, offset, origin, Shop, View};

/// Posts a page.
const PAGE: i64 = 12;

fn shown(p: &Post, root: &str) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or_default();
    v["url"] = json!(format!("{root}/blog/{}", p.handle));
    v["date"] = json!(p
        .published_at
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(10)
        .collect::<String>());
    v["tag_links"] = Value::Array(
        p.tags
            .iter()
            .map(|t| json!({ "name": t, "url": format!("{root}/blog?tag={}", encode(t)) }))
            .collect(),
    );
    v
}

/// The blog: published posts, newest first.
#[get("/blog", tag = "storefront")]
pub(crate) async fn blog_page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    params: ListParams,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let tag = params
        .get("tag")
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    let at = offset(&params);
    let (posts, total) = published_posts(&mut v.tx, &shop, tag.as_deref(), PAGE, at).await?;
    let root = v.lang.url("");
    let root = root.trim_end_matches('/');
    let base = match &tag {
        Some(t) => format!("{root}/blog?tag={}&", encode(t)),
        None => format!("{root}/blog?"),
    };
    if at > 0 {
        v.set(
            "prev_url",
            json!(format!("{base}offset={}", (at - PAGE).max(0))),
        );
    }
    if at + PAGE < total {
        v.set("next_url", json!(format!("{base}offset={}", at + PAGE)));
    }
    let title = v.data["t"]["blog"].as_str().unwrap_or("Blog").to_owned();
    v.set(
        "page_title",
        json!(match &tag {
            Some(t) => format!("{title}: {t}"),
            None => title,
        }),
    );
    v.set("tag", json!(tag));
    v.set("canonical", json!("/blog"));
    v.set("feed_url", json!(format!("{root}/blog/feed.xml")));
    v.set(
        "posts",
        Value::Array(posts.iter().map(|p| shown(p, root)).collect()),
    );
    v.render("blog", StatusCode::OK).await
}

/// One blog post.
#[get("/blog/{handle}", tag = "storefront")]
pub(crate) async fn post_page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(p) = published_post(&mut v.tx, &shop, &handle).await? else {
        return v.missing(&shop, &format!("/blog/{handle}")).await;
    };
    let root = v.lang.url("");
    let root = root.trim_end_matches('/').to_owned();
    v.set(
        "page_title",
        json!(p.seo_title.clone().unwrap_or_else(|| p.title.clone())),
    );
    let description = p
        .seo_description
        .clone()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| {
            if p.excerpt.is_empty() {
                soat_core::markdown::to_text(&p.body, 160)
            } else {
                p.excerpt.clone()
            }
        });
    if !description.is_empty() {
        v.set("meta_description", json!(description));
    }
    v.set("canonical", json!(format!("/blog/{}", p.handle)));
    v.set("blog_url", json!(format!("{root}/blog")));
    v.set("post", shown(&p, &root));
    v.render("post", StatusCode::OK).await
}

/// The blog's newest 20 posts as an Atom feed.
#[get("/blog/feed.xml", tag = "storefront")]
pub(crate) async fn feed(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let mut tx = shop.begin(&db).await?;
    let base = origin(&mut tx, &shop, &headers).await?;
    let (posts, _) = published_posts(&mut tx, &shop, None, 20, 0).await?;
    let (name,): (String,) = sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
        .bind(shop.tenant_id())
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    let updated = posts
        .first()
        .and_then(|p| p.published_at.clone())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".into());
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <feed xmlns=\"http://www.w3.org/2005/Atom\">\n\
         <title>{}</title>\n<id>{}/blog</id>\n\
         <link rel=\"alternate\" href=\"{}/blog\"/>\n\
         <link rel=\"self\" href=\"{}/blog/feed.xml\"/>\n<updated>{}</updated>\n",
        xml_escape(&name),
        xml_escape(&base),
        xml_escape(&base),
        xml_escape(&base),
        xml_escape(&updated)
    );
    for p in &posts {
        let url = format!("{base}/blog/{}", p.handle);
        let summary = if p.excerpt.is_empty() {
            soat_core::markdown::to_text(&p.body, 300)
        } else {
            p.excerpt.clone()
        };
        out.push_str(&format!(
            "<entry>\n<title>{}</title>\n<id>{}</id>\n<link rel=\"alternate\" href=\"{}\"/>\n\
             <published>{}</published>\n<updated>{}</updated>\n{}<summary>{}</summary>\n</entry>\n",
            xml_escape(&p.title),
            xml_escape(&url),
            xml_escape(&url),
            xml_escape(p.published_at.as_deref().unwrap_or_default()),
            xml_escape(&p.updated_at),
            if p.author.is_empty() {
                String::new()
            } else {
                format!("<author><name>{}</name></author>\n", xml_escape(&p.author))
            },
            xml_escape(&summary),
        ));
    }
    out.push_str("</feed>\n");
    Ok(text(out, "application/atom+xml; charset=utf-8"))
}
