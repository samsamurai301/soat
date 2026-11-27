//! The till: registers at the shop's locations, a sale screen for an open
//! session (scan or type SKUs, take cash or card, give change), receipts
//! and the cash count at the end of a shift.

use serde_json::{json, Value};
use soat_core::Currency;
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

/// The basket typed or scanned at the till: one `SKU` or `SKU quantity`
/// (or `quantity x SKU`) per line; the same SKU twice adds up.
fn basket(text: &str) -> std::result::Result<Vec<(String, i64)>, String> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let (sku, qty) = match words.as_slice() {
            [sku] => (*sku, "1"),
            [q, x, sku] if x.eq_ignore_ascii_case("x") => (*sku, *q),
            [sku, q] => (*sku, *q),
            _ => return Err(format!("“{line}” is not a SKU and a quantity.")),
        };
        let qty: i64 = qty
            .parse()
            .ok()
            .filter(|q| (1..=10000).contains(q))
            .ok_or_else(|| format!("“{line}”: the quantity is 1 to 10000."))?;
        match out.iter_mut().find(|(s, _)| s == sku) {
            Some((_, q)) => *q += qty,
            None => out.push((sku.to_owned(), qty)),
        }
    }
    if out.is_empty() {
        return Err("Scan or type a SKU first.".into());
    }
    Ok(out)
}

fn sale_body(lines: &[(String, i64)], discount: i64, tenders: Vec<Value>) -> Value {
    json!({
        "lines": lines.iter().map(|(sku, q)| json!({ "sku": sku, "quantity": q })).collect::<Vec<_>>(),
        "discount_minor": discount,
        "tenders": tenders,
    })
}

/// An amount typed at the till, in minor units; blank is 0.
fn amount(c: &Currency, text: &str) -> std::result::Result<i64, String> {
    let t = text.trim().replace(',', ".");
    if t.is_empty() {
        return Ok(0);
    }
    c.parse_decimal(&t)
        .filter(|v| *v >= 0)
        .ok_or_else(|| format!("“{text}” is not an amount like 20.00."))
}

/// The shop's registers, each with its open session, and a form for a new
/// one.
#[get("/admin/{shop}/pos", tag = "admin")]
pub(crate) async fn registers(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    Ok(registers_page(cx, None, StatusCode::OK).await)
}

async fn registers_page(mut cx: Ctx, error: Option<String>, status: StatusCode) -> Response {
    let (code, listed) = cx.call(Method::GET, "/api/pos/registers", None).await;
    match code {
        200 => {}
        404 => {
            cx.set("missing", json!(true));
            cx.set("page_title", json!("Point of sale"));
            return cx.render("pos", StatusCode::OK);
        }
        _ => return cx.failed(code, &listed),
    }
    let (_, locations) = cx.call(Method::GET, "/api/locations", None).await;
    cx.set("registers", listed["items"].clone());
    cx.set("locations", locations["items"].clone());
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Point of sale"));
    cx.render("pos", status)
}

/// Set up a register (a form post).
#[post("/admin/{shop}/pos", tag = "admin")]
pub(crate) async fn create_register(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let f = Fields::parse(&raw);
    let currency = f.get("currency").trim().to_uppercase();
    let b = json!({
        "name": f.get("name").trim(),
        "location_id": f.get("location_id"),
        "currency": Some(currency).filter(|c| !c.is_empty()),
    });
    let (status, out) = cx.call(Method::POST, "/api/pos/registers", Some(&b)).await;
    Ok(match status {
        201 => Redirect::to(&format!("/admin/{shop}/pos")).into_response(),
        401 => crate::to_login(),
        422 => registers_page(cx, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await,
        _ => cx.failed(status, &out),
    })
}

/// Open a session with the cash in the drawer, or take a register out of
/// use or back (form posts).
#[post("/admin/{shop}/pos/registers/{id}/{action}", tag = "admin")]
pub(crate) async fn register_act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let f = Fields::parse(&raw);
    let path = format!("/api/pos/registers/{}", encode(&id));
    match action.as_str() {
        "open" => {
            let c = Currency::parse(&f.get("currency"))
                .unwrap_or_else(|_| Currency::parse("EUR").expect("EUR is a currency"));
            let cash = match amount(&c, &f.get("cash")) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(registers_page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await)
                }
            };
            let b = json!({ "cash_minor": cash, "note": f.get("note") });
            let (status, out) = cx
                .call(Method::POST, &format!("{path}/sessions"), Some(&b))
                .await;
            Ok(match status {
                201 => Redirect::to(&format!(
                    "/admin/{shop}/pos/sessions/{}",
                    encode(out["id"].as_str().unwrap_or_default())
                ))
                .into_response(),
                401 => crate::to_login(),
                409 | 422 => registers_page(cx, Some(message(&out)), StatusCode::CONFLICT).await,
                _ => cx.failed(status, &out),
            })
        }
        "on" | "off" => {
            let b = json!({ "active": action == "on" });
            let (status, out) = cx.call(Method::PATCH, &path, Some(&b)).await;
            Ok(match status {
                200 => Redirect::to(&format!("/admin/{shop}/pos")).into_response(),
                401 => crate::to_login(),
                _ => cx.failed(status, &out),
            })
        }
        _ => Err(Error::not_found()),
    }
}

/// What the till screen shows besides the session: the basket, its total
/// and a message.
#[derive(Default)]
struct Till {
    lines: String,
    discount: String,
    quote: Option<Value>,
    error: Option<String>,
}

/// The till of a session: its report, and while it is open the sale
/// screen and the cash count to close it.
#[get("/admin/{shop}/pos/sessions/{id}", tag = "admin")]
pub(crate) async fn till(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    Ok(till_page(cx, &id, Till::default(), StatusCode::OK).await)
}

async fn till_page(mut cx: Ctx, id: &str, t: Till, status: StatusCode) -> Response {
    let s = page_or!(cx.get(&format!("/api/pos/sessions/{}", encode(id))).await);
    let currency = s["currency"].as_str().unwrap_or("EUR").to_owned();
    let m = |k: &str| money(s[k].as_i64().unwrap_or(0), &currency);
    let mut shown = s.clone();
    for k in [
        "opening_cash_minor",
        "total_minor",
        "cash_minor",
        "card_minor",
        "other_minor",
        "expected_cash_minor",
    ] {
        shown[k.trim_end_matches("_minor")] = json!(m(k));
    }
    if let Some(c) = s["counted_cash_minor"].as_i64() {
        shown["counted_cash"] = json!(money(c, &currency));
    }
    if let Some(d) = s["difference_minor"].as_i64() {
        shown["difference"] = json!(money(d, &currency));
        shown["short"] = json!(d < 0);
    }
    shown["open"] = json!(s["status"] == "open");
    cx.set("session", shown);
    cx.set("lines", json!(t.lines));
    cx.set("discount", json!(t.discount));
    if let Some(mut q) = t.quote {
        for l in q["lines"].as_array_mut().into_iter().flatten() {
            l["unit_price"] = json!(money(
                l["unit_price_minor"].as_i64().unwrap_or(0),
                &currency
            ));
            l["total"] = json!(money(l["total_minor"].as_i64().unwrap_or(0), &currency));
        }
        for k in ["subtotal", "discount", "tax", "total"] {
            q[k] = json!(money(
                q[format!("{k}_minor")].as_i64().unwrap_or(0),
                &currency
            ));
        }
        q["due"] = json!(Currency::parse(&currency)
            .map(|c| c.decimal(q["total_minor"].as_i64().unwrap_or(0)))
            .unwrap_or_default());
        cx.set("quote", q);
    }
    if let Some(e) = t.error {
        cx.set("error", json!(e));
    }
    cx.set(
        "page_title",
        json!(format!(
            "Till · {}",
            s["register_name"].as_str().unwrap_or("")
        )),
    );
    cx.render("pos_till", status)
}

/// Total the basket, take payment for it, or close the session with the
/// cash counted (form posts).
#[post("/admin/{shop}/pos/sessions/{id}/{action}", tag = "admin")]
pub(crate) async fn till_act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let f = Fields::parse(&raw);
    let path = format!("/api/pos/sessions/{}", encode(&id));
    let c = Currency::parse(&f.get("currency"))
        .unwrap_or_else(|_| Currency::parse("EUR").expect("EUR is a currency"));
    let mut t = Till {
        lines: f.get("lines"),
        discount: f.get("discount"),
        ..Till::default()
    };
    let unprocessable = StatusCode::UNPROCESSABLE_ENTITY;
    if action == "close" {
        let counted = match amount(&c, &f.get("cash")) {
            Ok(v) => v,
            Err(e) => {
                t.error = Some(e);
                return Ok(till_page(cx, &id, t, unprocessable).await);
            }
        };
        let b = json!({ "cash_minor": counted, "note": f.get("note") });
        let (status, out) = cx
            .call(Method::POST, &format!("{path}/close"), Some(&b))
            .await;
        return Ok(match status {
            200 => {
                Redirect::to(&format!("/admin/{shop}/pos/sessions/{}", encode(&id))).into_response()
            }
            401 => crate::to_login(),
            409 | 422 => {
                t.error = Some(message(&out));
                till_page(cx, &id, t, StatusCode::CONFLICT).await
            }
            _ => cx.failed(status, &out),
        });
    }
    if action != "total" && action != "pay" {
        return Err(Error::not_found());
    }
    let parsed = basket(&t.lines).and_then(|l| amount(&c, &t.discount).map(|d| (l, d)));
    let (lines, discount) = match parsed {
        Ok(v) => v,
        Err(e) => {
            t.error = Some(e);
            return Ok(till_page(cx, &id, t, unprocessable).await);
        }
    };
    let (qs, quote) = cx
        .call(
            Method::POST,
            &format!("{path}/quote"),
            Some(&sale_body(&lines, discount, vec![])),
        )
        .await;
    match qs {
        200 => t.quote = Some(quote.clone()),
        401 => return Ok(crate::to_login()),
        409 | 422 => {
            t.error = Some(message(&quote));
            return Ok(till_page(cx, &id, t, unprocessable).await);
        }
        _ => return Ok(cx.failed(qs, &quote)),
    }
    if action == "total" {
        return Ok(till_page(cx, &id, t, StatusCode::OK).await);
    }
    let total = quote["total_minor"].as_i64().unwrap_or(0);
    let (card, cash) = match (amount(&c, &f.get("card")), amount(&c, &f.get("cash"))) {
        (Ok(card), Ok(cash)) => (card, cash),
        (Err(e), _) | (_, Err(e)) => {
            t.error = Some(e);
            return Ok(till_page(cx, &id, t, unprocessable).await);
        }
    };
    // Paying by card alone: the card takes what is due.
    let card = if f.get("method") == "card" && card == 0 && cash == 0 {
        total
    } else {
        card
    };
    let mut tenders = Vec::new();
    if card > 0 {
        tenders.push(json!({ "kind": "card", "amount_minor": card }));
    }
    if cash > 0 {
        tenders.push(json!({ "kind": "cash", "amount_minor": cash }));
    }
    let (status, out) = cx
        .call(
            Method::POST,
            &format!("{path}/sales"),
            Some(&sale_body(&lines, discount, tenders)),
        )
        .await;
    Ok(match status {
        201 => Redirect::to(&format!(
            "/admin/{shop}/pos/sales/{}",
            encode(out["order"]["id"].as_str().unwrap_or_default())
        ))
        .into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            t.error = Some(message(&out));
            till_page(cx, &id, t, unprocessable).await
        }
        _ => cx.failed(status, &out),
    })
}

/// The receipt of a sale, to print or show.
#[get("/admin/{shop}/pos/sales/{order}", tag = "admin")]
pub(crate) async fn receipt(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, order)): Path<(String, String)>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let r = page!(cx.get(&format!("/api/pos/sales/{}", encode(&order))).await);
    let currency = r["order"]["currency"].as_str().unwrap_or("EUR").to_owned();
    let m = |v: &Value| money(v.as_i64().unwrap_or(0), &currency);
    let mut shown = r.clone();
    for k in ["cash", "card", "other", "change"] {
        shown[k] = json!(m(&r[format!("{k}_minor")]));
        shown[format!("has_{k}")] = json!(r[format!("{k}_minor")].as_i64().unwrap_or(0) > 0);
    }
    let o = &r["order"];
    shown["lines"] = Value::Array(
        o["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let mut l = l.clone();
                l["unit_price"] = json!(m(&l["unit_price_minor"]));
                l
            })
            .collect(),
    );
    for k in ["subtotal", "discount", "tax", "total"] {
        shown[k] = json!(m(&o[format!("{k}_minor")]));
    }
    shown["has_discount"] = json!(o["discount_minor"].as_i64().unwrap_or(0) > 0);
    cx.set("receipt", shown);
    cx.set("page_title", json!(format!("Receipt #{}", o["number"])));
    Ok(cx.render("pos_receipt", StatusCode::OK))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baskets() {
        assert_eq!(
            basket("MUG\nTEE-M 2\n3 x CAP\nMUG").unwrap(),
            [("MUG".into(), 2), ("TEE-M".into(), 2), ("CAP".into(), 3)]
        );
        assert!(basket("").is_err());
        assert!(basket("MUG 0").is_err());
        assert!(basket("a b c d").is_err());
        let eur = Currency::parse("EUR").unwrap();
        assert_eq!(amount(&eur, "20,50"), Ok(2050));
        assert_eq!(amount(&eur, " "), Ok(0));
        assert!(amount(&eur, "-1").is_err());
    }
}
