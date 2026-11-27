//! The unsubscribe link of campaign emails (feature `marketing`). Opening
//! it asks to confirm, so mail scanners that open links change nothing;
//! the one-click `POST` of mail apps (`List-Unsubscribe-Post`) and the
//! page's button unsubscribe.

use serde_json::json;
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use crate::View;

/// The link's query.
#[model]
pub struct UnsubscribeQuery {
    pub token: Option<String>,
}

/// The page an unsubscribe link opens: a button to confirm.
#[get("/unsubscribe", tag = "storefront")]
pub(crate) async fn page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Query(q): Query<UnsubscribeQuery>,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    v.set("page_title", json!(v.t("unsubscribe")));
    v.set("token", json!(q.token.unwrap_or_default()));
    v.render("unsubscribe", StatusCode::OK).await
}

/// The confirm form's field; mail apps send the token in the query.
#[model]
pub struct UnsubscribeForm {
    pub token: Option<String>,
}

/// Unsubscribe: from the page's button, or a mail app's one-click post.
#[post("/unsubscribe", tag = "storefront")]
pub(crate) async fn confirm(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Query(q): Query<UnsubscribeQuery>,
    Form(f): Form<UnsubscribeForm>,
) -> Result<Response> {
    let token = q
        .token
        .filter(|t| !t.is_empty())
        .or(f.token)
        .unwrap_or_default();
    let mut v = View::open(&db, &shop, &headers).await?;
    let status = match soat_marketing::unsubscribe(&mut v.tx, &shop, &token).await {
        Ok(_) => {
            v.set("done", json!(true));
            StatusCode::OK
        }
        Err(e) if e.status() == StatusCode::NOT_FOUND => {
            v.set("invalid", json!(true));
            StatusCode::NOT_FOUND
        }
        Err(e) => return Err(e),
    };
    v.set("page_title", json!(v.t("unsubscribe")));
    v.render("unsubscribe", status).await
}
