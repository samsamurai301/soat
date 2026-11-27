//! Subscriptions on the storefront (feature `subscriptions`): a
//! subscribe form on product pages with plans, and the account's list
//! with pause, resume, skip and cancel.

use serde_json::{json, Value};
use soat_checkout::TaxProviders;
use soat_payments::Providers;
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use crate::{put_cookie, root_of, upper_first, View, ORDER_COOKIE};

/// The product's plans, for the subscribe form on its page.
pub(crate) async fn on_product(v: &mut View, shop: &Shop, id: &str) -> Result<()> {
    let plans: Vec<Value> = soat_subscriptions::plans_for(&mut v.tx, shop, id)
        .await?
        .into_iter()
        .filter(|p| p.active)
        .map(|p| {
            let unit = v.t(&format!(
                "every_{}{}",
                p.interval_unit,
                if p.interval_count == 1 { "" } else { "s" }
            ));
            json!({
                "id": p.id,
                "name": p.name,
                "every": unit.replace("{n}", &p.interval_count.to_string()),
                "discount": (p.discount_bps > 0).then(|| {
                    format!("{}%", trim_percent(p.discount_bps))
                }),
            })
        })
        .collect();
    if !plans.is_empty() {
        v.set("subscription_plans", Value::Array(plans));
    }
    Ok(())
}

/// Basis points as a percentage without needless zeros: 1000 is "10",
/// 750 is "7.5".
fn trim_percent(bps: i64) -> String {
    let s = format!("{}.{:02}", bps / 100, bps % 100);
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The subscribe form.
#[model]
pub struct SubscribeForm {
    pub plan_id: String,
    pub variant_id: Option<String>,
    pub quantity: Option<String>,
}

/// Subscribe to a product's plan: the first order is placed now, to the
/// account's saved address. Others are sent to sign in.
#[post("/products/{handle}/subscribe", tag = "storefront")]
#[allow(clippy::too_many_arguments)] // one per extractor
pub(crate) async fn subscribe(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    taxes: State<TaxProviders>,
    providers: State<Providers>,
    Path(handle): Path<String>,
    Form(f): Form<SubscribeForm>,
) -> Result<Response> {
    let root = root_of(&headers);
    let v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    v.tx.commit().await?;
    let quantity = f
        .quantity
        .as_deref()
        .and_then(|q| q.trim().parse::<i32>().ok())
        .unwrap_or(1)
        .clamp(1, 99);
    let s = soat_subscriptions::Subscribe {
        plan_id: f.plan_id,
        item_id: f.variant_id.filter(|v| !v.is_empty()),
        quantity: Some(quantity),
        shipping_address: None,
        billing_address: None,
        payment_method_id: None,
    };
    match soat_subscriptions::start(&db, &shop, &taxes, &providers, &customer, s).await {
        Ok(done) => {
            // The first order's page, where the buyer pays for it.
            let mut res = Redirect::to(&format!("{root}/order")).into_response();
            put_cookie(
                &mut res,
                ORDER_COOKIE,
                &format!("{}.{}", done.order_id, done.order_token),
                2_592_000,
                conn.secure,
            );
            Ok(res)
        }
        Err(e) if e.status() == StatusCode::NOT_FOUND => {
            let v = View::open(&db, &shop, &headers).await?;
            v.missing(&shop, &format!("/products/{handle}")).await
        }
        Err(e) if e.status().is_client_error() => {
            let v = View::open(&db, &shop, &headers).await?;
            let msg = if e.code() == Some("invalid_subscription") && e.message().contains("address")
            {
                v.t("subscription_needs_address")
            } else {
                upper_first(e.message())
            };
            show(v, &shop, &customer, None, Some(msg), e.status()).await
        }
        Err(e) => Err(e),
    }
}

async fn show(
    mut v: View,
    shop: &Shop,
    customer: &str,
    notice: Option<String>,
    error: Option<String>,
    status: StatusCode,
) -> Result<Response> {
    let items: Vec<Value> = soat_subscriptions::of_customer(&mut v.tx, shop, customer)
        .await?
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "plan": s.plan_name,
                "quantity": s.quantity,
                "status": v.t(&format!("subscription_{}", s.status)),
                "active": s.status == "active",
                "paused": s.status == "paused",
                "canceled": s.status == "canceled",
                "next": s.next_order_at.get(..10).unwrap_or(&s.next_order_at),
                "orders": s.orders_count,
            })
        })
        .collect();
    v.set("page_title", json!(v.t("subscriptions")));
    v.set("items", Value::Array(items));
    v.set("notice", json!(notice));
    v.set("error", json!(error));
    v.render("subscriptions", status).await
}

/// The account page's query after a change.
#[model]
pub struct SubscriptionsQuery {
    pub done: Option<String>,
}

/// The signed-in buyer's subscriptions.
#[get("/account/subscriptions", tag = "storefront")]
pub(crate) async fn page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Query(q): Query<SubscriptionsQuery>,
) -> Result<Response> {
    let v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{}/account/login", v.lang.root)).into_response());
    };
    let notice = q.done.is_some().then(|| v.t("saved"));
    show(v, &shop, &customer, notice, None, StatusCode::OK).await
}

/// Pause, resume, skip the next order of, or cancel one of the buyer's
/// subscriptions (a form post).
#[post("/account/subscriptions/{id}/{action}", tag = "storefront")]
pub(crate) async fn act(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
) -> Result<Response> {
    let root = root_of(&headers);
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    let mine = soat_subscriptions::of_customer(&mut v.tx, &shop, &customer)
        .await?
        .into_iter()
        .any(|s| s.id == id);
    if !mine || !["pause", "resume", "skip", "cancel"].contains(&action.as_str()) {
        return v
            .missing(&shop, &format!("/account/subscriptions/{id}"))
            .await;
    }
    match soat_subscriptions::change(&mut v.tx, &shop, &id, &action).await {
        Ok(_) => {
            v.tx.commit().await?;
            Ok(
                Redirect::to(&format!("{root}/account/subscriptions?done={action}"))
                    .into_response(),
            )
        }
        Err(e) if e.status() == StatusCode::CONFLICT => {
            let v = View::open(&db, &shop, &headers).await?;
            show(
                v,
                &shop,
                &customer,
                None,
                Some(upper_first(e.message())),
                e.status(),
            )
            .await
        }
        Err(e) => Err(e),
    }
}
