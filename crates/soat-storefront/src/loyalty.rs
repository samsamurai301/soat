//! Loyalty points on the storefront (feature `loyalty`): the account's
//! points page, turning points into store credit, and referral codes.

use serde_json::{json, Value};
use soat_customers::accounts::SignedIn;
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use crate::{money, root_of, upper_first, View};

/// Enters a friend's code for a customer who just registered. A code
/// that does not work is left out quietly: the account is made either way.
pub(crate) async fn after_register(db: &Db, shop: &Shop, token: &str, code: &str) {
    let signed_in = SignedIn::new(shop.clone(), token);
    let Ok((mut tx, customer)) = signed_in.begin(db).await else {
        return;
    };
    if soat_loyalty::enter_code(&mut tx, shop, &customer, code)
        .await
        .is_ok()
    {
        let _ = tx.commit().await;
    }
}

/// Fills in the page for the signed-in customer and renders it.
async fn show(
    mut v: View,
    shop: &Shop,
    customer: &str,
    notice: Option<String>,
    error: Option<String>,
    status: StatusCode,
) -> Result<Response> {
    let m = soat_loyalty::mine(&mut v.tx, shop, customer).await?;
    let currency = soat_catalog::storefront::main_currency(&mut v.tx, shop).await?;
    let worth = |points: i64| -> Option<String> {
        let c = soat_core::Currency::parse(currency.as_deref()?).ok()?;
        let unit = 10_i128.pow(c.decimals());
        let minor = i128::from(points) * unit / i128::from(m.program.points_per_credit.max(1));
        Some(money(i64::try_from(minor).ok()?, c.as_str()))
    };
    let entries: Vec<Value> = m
        .entries
        .iter()
        .map(|e| {
            json!({
                "date": e.created_at.get(..10).unwrap_or(&e.created_at),
                "kind": v.t(&format!("points_kind_{}", e.kind)),
                "points": if e.points > 0 { format!("+{}", e.points) } else { e.points.to_string() },
                "note": e.note,
            })
        })
        .collect();
    let can_redeem = m.program.enabled && m.balance >= m.program.min_redeem && currency.is_some();
    v.set("page_title", json!(v.t("points")));
    v.set(
        "loyalty",
        json!({
            "on": m.program.enabled,
            "balance": m.balance,
            "worth": worth(m.balance),
            "min_redeem": m.program.min_redeem,
            "can_redeem": can_redeem,
            "currency": currency,
            "points_per_unit": m.program.points_per_unit,
            "referral_code": m.referral_code,
            "referrer_points": m.program.referrer_points,
            "referee_points": m.program.referee_points,
            "can_enter_referral": m.can_enter_referral,
            "entries": entries,
        }),
    );
    v.set("notice", json!(notice));
    v.set("error", json!(error));
    v.render("loyalty", status).await
}

/// The query of the points page after a change.
#[model]
pub struct LoyaltyQuery {
    pub done: Option<String>,
}

/// The signed-in buyer's points, their history and their referral code.
#[get("/account/loyalty", tag = "storefront")]
pub(crate) async fn page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Query(q): Query<LoyaltyQuery>,
) -> Result<Response> {
    let v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{}/account/login", v.lang.root)).into_response());
    };
    let notice = match q.done.as_deref() {
        Some("redeemed") => Some(v.t("points_redeemed")),
        Some("referred") => Some(v.t("referral_entered")),
        _ => None,
    };
    show(v, &shop, &customer, notice, None, StatusCode::OK).await
}

/// The redeem form's field.
#[model]
pub struct RedeemForm {
    pub points: String,
}

/// Turn points into store credit in the shop's main currency (a form
/// post).
#[post("/account/loyalty/redeem", tag = "storefront")]
pub(crate) async fn redeem(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Form(f): Form<RedeemForm>,
) -> Result<Response> {
    let root = root_of(&headers);
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    let currency = soat_catalog::storefront::main_currency(&mut v.tx, &shop)
        .await?
        .unwrap_or_default();
    let points = f.points.trim().parse::<i64>().unwrap_or(0);
    let outcome = soat_loyalty::redeem(&mut v.tx, &shop, &customer, points, &currency).await;
    match outcome {
        Ok(_) => {
            v.tx.commit().await?;
            Ok(Redirect::to(&format!("{root}/account/loyalty?done=redeemed")).into_response())
        }
        Err(e) if e.status() == StatusCode::UNPROCESSABLE_ENTITY => {
            let v = View::open(&db, &shop, &headers).await?;
            let msg = upper_first(e.message());
            show(v, &shop, &customer, None, Some(msg), e.status()).await
        }
        Err(e) => Err(e),
    }
}

/// The referral form's field.
#[model]
pub struct ReferralForm {
    pub code: String,
}

/// Enter a friend's referral code (a form post).
#[post("/account/loyalty/referral", tag = "storefront")]
pub(crate) async fn referral(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Form(f): Form<ReferralForm>,
) -> Result<Response> {
    let root = root_of(&headers);
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    match soat_loyalty::enter_code(&mut v.tx, &shop, &customer, &f.code).await {
        Ok(()) => {
            v.tx.commit().await?;
            Ok(Redirect::to(&format!("{root}/account/loyalty?done=referred")).into_response())
        }
        Err(e) if e.status() == StatusCode::UNPROCESSABLE_ENTITY => {
            let v = View::open(&db, &shop, &headers).await?;
            let msg = upper_first(e.message());
            show(v, &shop, &customer, None, Some(msg), e.status()).await
        }
        Err(e) => Err(e),
    }
}
