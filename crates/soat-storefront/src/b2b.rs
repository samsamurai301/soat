//! B2B on the storefront (feature `b2b`): quick order by SKU, quotes
//! (ask, see the offer, accept or decline) and the buyer's company page.

use serde_json::{json, Value};
use soat_cart::storefront::{self as carts, CartToken};
use soat_checkout::TaxProviders;
use soat_payments::Providers;
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use crate::{cart_cookie, money, put_cookie, root_of, set_cookie, upper_first, View, ORDER_COOKIE};

/// The signed-in buyer, or a redirect to sign in.
async fn signed_in(v: View) -> Result<std::result::Result<(View, String), Response>> {
    match v.customer.clone() {
        Some(c) => Ok(Ok((v, c))),
        None => {
            let to = format!("{}/account/login", v.lang.root);
            v.tx.commit().await?;
            Ok(Err(Redirect::to(&to).into_response()))
        }
    }
}

/// The quick order form.
#[model]
pub struct QuickOrderForm {
    /// `SKU quantity` per line.
    pub lines: Option<String>,
    /// `quote` asks for a price; else the lines go in the cart.
    pub action: Option<String>,
    pub note: Option<String>,
}

async fn quick_page(
    mut v: View,
    typed: &str,
    error: Option<String>,
    status: StatusCode,
) -> Result<Response> {
    v.set("page_title", json!(v.t("quick_order")));
    v.set("typed", json!(typed));
    v.set("error", json!(error));
    v.render("quick_order", status).await
}

/// A form to order many items at once by SKU, or ask for a price on them.
#[get("/account/quick-order", tag = "storefront")]
pub(crate) async fn quick_order(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let v = View::open(&db, &shop, &headers).await?;
    let (v, _) = match signed_in(v).await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    quick_page(v, "", None, StatusCode::OK).await
}

/// Puts the quick order's lines in the cart (all or none), or asks the
/// shop for a price on them (a form post).
#[post("/account/quick-order", tag = "storefront")]
pub(crate) async fn quick_order_post(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    taxes: State<TaxProviders>,
    Form(f): Form<QuickOrderForm>,
) -> Result<Response> {
    let root = root_of(&headers);
    let v = View::open(&db, &shop, &headers).await?;
    let (mut v, customer) = match signed_in(v).await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    let text = f.lines.unwrap_or_default();
    let refuse =
        |v: View, msg: String| quick_page(v, &text, Some(msg), StatusCode::UNPROCESSABLE_ENTITY);
    let lines = match soat_b2b::parse_quick_order(&text) {
        Ok(l) if l.is_empty() => {
            let msg = v.t("quick_order_empty");
            return refuse(v, msg).await;
        }
        Ok(l) => l,
        Err((n, why)) => {
            let msg = format!("{} {n}: {why}", v.t("line"));
            return refuse(v, msg).await;
        }
    };
    let mut items = Vec::with_capacity(lines.len());
    let mut unknown = Vec::new();
    for (sku, quantity) in &lines {
        match soat_b2b::item_by_sku(&mut v.tx, &shop, sku).await? {
            Some((item, _)) => items.push((sku.clone(), item, *quantity)),
            None => unknown.push(sku.clone()),
        }
    }
    if !unknown.is_empty() {
        let msg = format!("{} {}", v.t("unknown_skus"), unknown.join(", "));
        return refuse(v, msg).await;
    }
    if f.action.as_deref() == Some("quote") {
        let request = soat_b2b::QuoteRequest {
            lines: items
                .iter()
                .map(|(_, item, quantity)| soat_b2b::QuoteLine {
                    item_id: item.clone(),
                    quantity: *quantity,
                    title: String::new(),
                    sku: String::new(),
                })
                .collect(),
            note: f.note.filter(|n| !n.trim().is_empty()),
        };
        let q = soat_b2b::request(&mut v.tx, &shop, &taxes, &customer, request).await?;
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{root}/account/quotes/{}", q.id)).into_response());
    }
    // Into the buyer's cart, or a new one in the first item's currency.
    let held = match cart_cookie(&headers) {
        Some((id, token)) => carts::held(&mut v.tx, &shop, &id, &token)
            .await?
            .map(|c| (c.id, token)),
        None => None,
    };
    let (mut tx, cart, cookie) = match held {
        Some((id, token)) => {
            let holder = CartToken::new(shop.clone(), &token);
            let (tx, id) = holder.open(&db, &id).await?;
            (tx, id, None)
        }
        None => {
            let first = soat_catalog::stock_item(&mut v.tx, &shop, &items[0].1)
                .await?
                .ok_or_else(Error::not_found)?;
            let mut tx = shop.begin(&db).await?;
            let (token, cart) = carts::create(&mut tx, &shop, &first.currency, None).await?;
            (tx, cart.id.clone(), Some(format!("{}.{token}", cart.id)))
        }
    };
    // The buyer's own prices (their company's group) apply to their cart.
    carts::attach(&mut tx, &shop, &cart, &customer).await?;
    for (sku, item, quantity) in &items {
        if let Err(e) = carts::add(&mut tx, &shop, &cart, item, *quantity).await {
            drop(tx);
            let msg = format!("{sku}: {}", upper_first(e.message()));
            return refuse(v, msg).await;
        }
    }
    tx.commit().await?;
    v.tx.commit().await?;
    let mut res = Redirect::to(&format!("{root}/cart")).into_response();
    if let Some(c) = cookie {
        set_cookie(&mut res, &c, conn.secure);
    }
    Ok(res)
}

fn quote_status(v: &View, status: &str) -> String {
    v.t(&format!("quote_{status}"))
}

/// The signed-in buyer's quotes.
#[get("/account/quotes", tag = "storefront")]
pub(crate) async fn quotes(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let v = View::open(&db, &shop, &headers).await?;
    let (mut v, customer) = match signed_in(v).await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    let list = soat_b2b::of_customer(&mut v.tx, &shop, &customer).await?;
    let items: Vec<Value> = list
        .iter()
        .map(|q| {
            json!({
                "id": q.id,
                "date": q.created_at.get(..10).unwrap_or(&q.created_at),
                "lines": q.lines.len(),
                "first": q.lines.first().map(|l| l.title.clone()),
                "status": quote_status(&v, &q.status),
            })
        })
        .collect();
    v.set("page_title", json!(v.t("quotes")));
    v.set("items", Value::Array(items));
    v.render("quotes", StatusCode::OK).await
}

async fn quote_page(
    mut v: View,
    shop: &Shop,
    taxes: &TaxProviders,
    customer: &str,
    id: &str,
    error: Option<String>,
    status: StatusCode,
) -> Result<Response> {
    let q = soat_b2b::my(&mut v.tx, shop, taxes, customer, id).await?;
    let offer = q.offer.as_ref().map(|d| {
        json!({
            "lines": d.lines.iter().map(|l| json!({
                "title": l.title, "sku": l.sku, "quantity": l.quantity,
                "unit_price": money(l.unit_price_minor, &d.currency),
            })).collect::<Vec<_>>(),
            "subtotal": money(d.subtotal_minor, &d.currency),
            "discount": (d.discount_minor > 0).then(|| money(d.discount_minor, &d.currency)),
            "shipping": money(d.shipping_price_minor, &d.currency),
            "tax": money(d.tax_minor, &d.currency),
            "total": money(d.total_minor, &d.currency),
        })
    });
    let offered = q.status == "offered";
    v.set("page_title", json!(v.t("quote")));
    v.set(
        "quote",
        json!({
            "id": q.id,
            "status": quote_status(&v, &q.status),
            "open": q.status == "requested" || offered,
            "offered": offered,
            "lines": q.lines,
            "note": q.note,
            "reply": q.reply,
            "expires": q.expires_at.as_deref().and_then(|e| e.get(..10)),
            "offer": offer,
        }),
    );
    v.set("error", json!(error));
    v.render("quote", status).await
}

/// One of the buyer's quotes, with the shop's offer.
#[get("/account/quotes/{id}", tag = "storefront")]
pub(crate) async fn quote(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
) -> Result<Response> {
    let v = View::open(&db, &shop, &headers).await?;
    let (v, customer) = match signed_in(v).await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    match quote_page(v, &shop, &taxes, &customer, &id, None, StatusCode::OK).await {
        Err(e) if e.status() == StatusCode::NOT_FOUND => {
            let v = View::open(&db, &shop, &headers).await?;
            v.missing(&shop, &format!("/account/quotes/{id}")).await
        }
        other => other,
    }
}

/// Accept the offer (its order is placed and shown) or decline it (a
/// form post).
#[post("/account/quotes/{id}/{action}", tag = "storefront")]
#[allow(clippy::too_many_arguments)] // one per extractor
pub(crate) async fn quote_act(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    taxes: State<TaxProviders>,
    providers: State<Providers>,
    Path((id, action)): Path<(String, String)>,
) -> Result<Response> {
    let root = root_of(&headers);
    let v = View::open(&db, &shop, &headers).await?;
    let (mut v, customer) = match signed_in(v).await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    let outcome = match action.as_str() {
        "decline" => {
            let out = soat_b2b::decline(&mut v.tx, &shop, &taxes, &customer, &id).await;
            if out.is_ok() {
                v.tx.commit().await?;
                return Ok(Redirect::to(&format!("{root}/account/quotes/{id}")).into_response());
            }
            out.map(|_| ())
        }
        "accept" => {
            v.tx.commit().await?;
            let a = soat_b2b::Acceptance {
                payment_method_id: None,
                return_url: None,
            };
            match soat_b2b::accept(&db, &shop, &taxes, &providers, &customer, &id, a).await {
                Ok(done) => {
                    let mut res = Redirect::to(&format!("{root}/order")).into_response();
                    put_cookie(
                        &mut res,
                        ORDER_COOKIE,
                        &format!("{}.{}", done.order_id, done.order_token),
                        2_592_000,
                        conn.secure,
                    );
                    return Ok(res);
                }
                Err(e) => {
                    v = View::open(&db, &shop, &headers).await?;
                    Err(e)
                }
            }
        }
        _ => return v.missing(&shop, &format!("/account/quotes/{id}")).await,
    };
    match outcome {
        Err(e) if e.status() == StatusCode::NOT_FOUND => {
            v.missing(&shop, &format!("/account/quotes/{id}")).await
        }
        Err(e) if e.status().is_client_error() => {
            let v = View::open(&db, &shop, &headers).await?;
            let msg = match e.code() {
                Some(c @ ("quote_expired" | "company_on_hold" | "credit_limit_exceeded")) => v.t(c),
                _ => upper_first(e.message()),
            };
            quote_page(v, &shop, &taxes, &customer, &id, Some(msg), e.status()).await
        }
        Err(e) => Err(e),
        Ok(()) => Ok(Redirect::to(&format!("{root}/account/quotes/{id}")).into_response()),
    }
}

/// The buyer's company: its terms and, for its admins, what it owes and
/// its people.
#[get("/account/company", tag = "storefront")]
pub(crate) async fn company(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let v = View::open(&db, &shop, &headers).await?;
    let (mut v, customer) = match signed_in(v).await? {
        Ok(x) => x,
        Err(r) => return Ok(r),
    };
    let Some((c, role)) = soat_b2b::company_of(&mut v.tx, &shop, &customer).await? else {
        return v.missing(&shop, "/account/company").await;
    };
    let admin = role == "admin";
    let balance = if admin {
        let b = soat_b2b::balance(&mut v.tx, &shop, &c).await?;
        Some(json!({
            "open": money(b.open_minor, &b.currency),
            "overdue": (b.overdue_minor > 0).then(|| money(b.overdue_minor, &b.currency)),
            "limit": b.credit_limit_minor.map(|l| money(l, &b.currency)),
            "available": b.available_minor.map(|l| money(l, &b.currency)),
            "orders": b.orders.iter().map(|o| json!({
                "number": o.number,
                "total": money(o.total_minor, &o.currency),
                "placed": o.placed_at.get(..10),
                "due": o.due_at.get(..10),
                "state": v.t(&format!("ledger_{}", o.state)),
                "overdue": o.state == "overdue",
            })).collect::<Vec<_>>(),
        }))
    } else {
        None
    };
    v.set("page_title", json!(c.name));
    v.set(
        "company",
        json!({
            "name": c.name,
            "vat_number": c.vat_number,
            "terms": c.payment_terms_days,
            "on_hold": c.status == "on_hold",
            "admin": admin,
            "balance": balance,
        }),
    );
    v.render("company", StatusCode::OK).await
}
