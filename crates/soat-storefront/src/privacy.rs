//! A customer's own data on the storefront (feature `privacy`): download
//! it, and ask the shop to erase it.

use soat_module::PersonalDataMap;
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use crate::account::CUSTOMER_COOKIE;
use crate::{cookie, root_of};
use soat_customers::accounts::SignedIn;

/// The signed-in customer's data as a JSON file; others are sent to sign
/// in.
#[get("/account/personal-data", tag = "storefront")]
pub(crate) async fn download(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    map: State<PersonalDataMap>,
) -> Result<Response> {
    let login = || Redirect::to(&format!("{}/account/login", root_of(&headers))).into_response();
    let Some(token) = cookie(&headers, CUSTOMER_COOKIE) else {
        return Ok(login());
    };
    let Ok((mut tx, customer)) = SignedIn::new(shop.clone(), &token).begin(&db).await else {
        return Ok(login());
    };
    let out = soat_compliance::personal_data_of(&mut tx, &shop, &map, &customer).await?;
    tx.commit().await?;
    let mut res = serde_json::to_string_pretty(&out)
        .unwrap_or_default()
        .into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"my-data.json\""),
    );
    Ok(res)
}

/// Ask the shop to erase the signed-in customer (a form post).
#[post("/account/erasure-request", tag = "storefront")]
pub(crate) async fn ask(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let root = root_of(&headers);
    let Some(token) = cookie(&headers, CUSTOMER_COOKIE) else {
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    let Ok((mut tx, customer)) = SignedIn::new(shop.clone(), &token).begin(&db).await else {
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    soat_compliance::ask_erasure(&mut tx, &shop, &customer).await?;
    tx.commit().await?;
    Ok(Redirect::to(&format!("{root}/account?erasure=1")).into_response())
}
