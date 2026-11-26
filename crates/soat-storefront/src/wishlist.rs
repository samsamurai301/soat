//! Wishlists on the storefront (feature `wishlists`): a save button on
//! product pages and the list in the account.

use serde_json::{json, Value};
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use crate::{money, root_of, View};

/// Whether the signed-in buyer saved product `id`, for the product page.
pub(crate) async fn on_product(v: &mut View, shop: &Shop, id: &str) -> Result<()> {
    let saved = match v.customer.clone() {
        Some(c) => soat_wishlists::saved(&mut v.tx, shop, &c, id).await?,
        None => false,
    };
    v.set("wishlist", json!({ "on": true, "saved": saved }));
    Ok(())
}

/// The save button: saves the product, or removes it when it is saved.
/// Others are sent to sign in.
#[post("/products/{handle}/wishlist", tag = "storefront")]
pub(crate) async fn toggle(
    db: Db,
    shop: Shop,
    storage: zapline::storage::Storage,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> Result<Response> {
    let root = root_of(&headers);
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    let Some(p) =
        soat_catalog::storefront::product_by_handle(&mut v.tx, &shop, &storage, &handle).await?
    else {
        return v.missing(&shop, &format!("/products/{handle}")).await;
    };
    if soat_wishlists::saved(&mut v.tx, &shop, &customer, &p.id).await? {
        soat_wishlists::remove(&mut v.tx, &shop, &customer, &p.id).await?;
    } else {
        soat_wishlists::add(&mut v.tx, &shop, &customer, &p.id).await?;
    }
    v.tx.commit().await?;
    Ok(Redirect::to(&format!("{root}/products/{}", p.handle)).into_response())
}

/// The signed-in buyer's saved products.
#[get("/account/wishlist", tag = "storefront")]
pub(crate) async fn page(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{}/account/login", v.lang.root)).into_response());
    };
    let items: Vec<Value> = soat_wishlists::items(&mut v.tx, &shop, &customer)
        .await?
        .into_iter()
        .map(|i| {
            json!({
                "handle": i.handle,
                "title": i.title,
                "price": money(i.price_minor, &i.currency),
            })
        })
        .collect();
    let title = v.t("wishlist");
    v.set("page_title", json!(title));
    v.set("items", Value::Array(items));
    v.render("wishlist", StatusCode::OK).await
}
