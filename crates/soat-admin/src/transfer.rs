//! CSV import and export of products (a row per variant, with stock) and
//! customers. Every row goes through the API, so roles and events apply as
//! for any other change.

use serde_json::{json, Map, Value};
use soat_core::csv::{write, Table};
use soat_core::Currency;
use soat_core::{shopify, shopware, woocommerce};
use zapline::db::Db;
use zapline::multipart::Multipart;
use zapline::prelude::*;

use crate::settings::{amount, Fields};
use crate::util::encode;
use crate::{Ctx, Page, Shared};

/// Rows one import takes at most.
const MAX_ROWS: usize = 5000;

/// The product file's columns, in order.
pub(crate) const PRODUCT_COLUMNS: [&str; 17] = [
    "handle",
    "title",
    "description",
    "status",
    "tags",
    "sku",
    "price",
    "currency",
    "option1_name",
    "option1_value",
    "option2_name",
    "option2_value",
    "option3_name",
    "option3_value",
    "weight_grams",
    "tax_class",
    "stock",
];

/// The customer file's columns, in order.
pub(crate) const CUSTOMER_COLUMNS: [&str; 7] = [
    "email",
    "first_name",
    "last_name",
    "phone",
    "accepts_marketing",
    "tags",
    "note",
];

async fn page(
    mut cx: Ctx,
    result: Option<Value>,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let (got, shopify) = cx.call(Method::GET, "/api/imports/shopify", None).await;
    if got == 200 {
        cx.set("shopify", shopify);
    }
    cx.set("page_title", json!("Import and export"));
    cx.set("product_columns", json!(PRODUCT_COLUMNS.join(", ")));
    cx.set("customer_columns", json!(CUSTOMER_COLUMNS.join(", ")));
    if let Some(r) = result {
        cx.set("result", r);
    }
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("transfer", status)
}

/// Import and export products and customers as CSV.
#[get("/admin/{shop}/transfer", tag = "admin")]
pub(crate) async fn show(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    Ok(page(cx, None, None, StatusCode::OK).await)
}

fn csv_file(name: &str, rows: &[Vec<String>]) -> Response {
    let mut res = write(rows).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

/// Every item of a paged list, 200 at a time.
async fn all(cx: &Ctx, path: &str) -> Page<Vec<Value>> {
    let mut out = Vec::new();
    loop {
        let sep = if path.contains('?') { '&' } else { '?' };
        let page = cx
            .get(&format!("{path}{sep}limit=200&offset={}", out.len()))
            .await?;
        let items = page["items"].as_array().cloned().unwrap_or_default();
        let total = page["total"].as_u64().unwrap_or(0) as usize;
        let done = items.is_empty();
        out.extend(items);
        if done || out.len() >= total {
            return Ok(out);
        }
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

fn decimal(minor: &Value, currency: &str) -> String {
    let m = minor.as_i64().unwrap_or(0);
    Currency::parse(currency).map_or_else(|_| m.to_string(), |c| c.decimal(m))
}

fn tags(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The shop's products as CSV: a row per variant (one for a product without
/// variants), with the units on hand.
#[get("/admin/{shop}/transfer/products.csv", tag = "admin")]
pub(crate) async fn export_products(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let products = page!(all(&cx, "/api/products?sort=id").await);
    let mut rows = vec![PRODUCT_COLUMNS.iter().map(|c| (*c).to_owned()).collect()];
    for p in products {
        let id = p["id"].as_str().unwrap_or_default();
        let currency = p["currency"].as_str().unwrap_or_default();
        let options = page!(cx.get(&format!("/api/products/{id}/options")).await);
        let names: Vec<String> = options
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| text(&o["name"]))
            .collect();
        let variants = page!(cx.get(&format!("/api/products/{id}/variants")).await);
        let variants = variants.as_array().cloned().unwrap_or_default();
        let items: Vec<(String, Value, Vec<String>)> = if variants.is_empty() {
            vec![(id.to_owned(), p["price_minor"].clone(), Vec::new())]
        } else {
            variants
                .iter()
                .map(|v| {
                    let values = v["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(text)
                        .collect();
                    (text(&v["id"]), v["price_minor"].clone(), values)
                })
                .collect()
        };
        for (i, (item, price, values)) in items.iter().enumerate() {
            let first = i == 0;
            let (_, stock) = cx
                .call(
                    Method::GET,
                    &format!("/api/inventory/{}", encode(item)),
                    None,
                )
                .await;
            let sku = if variants.is_empty() {
                text(&p["sku"])
            } else {
                text(&variants[i]["sku"])
            };
            let mut row = vec![
                text(&p["handle"]),
                if first {
                    text(&p["title"])
                } else {
                    String::new()
                },
                if first {
                    text(&p["description"])
                } else {
                    String::new()
                },
                if first {
                    text(&p["status"])
                } else {
                    String::new()
                },
                if first {
                    tags(&p["tags"])
                } else {
                    String::new()
                },
                sku,
                decimal(price, currency),
                currency.to_owned(),
            ];
            for n in 0..3 {
                row.push(if first {
                    names.get(n).cloned().unwrap_or_default()
                } else {
                    String::new()
                });
                row.push(values.get(n).cloned().unwrap_or_default());
            }
            row.push(if first {
                text(&p["weight_grams"])
            } else {
                String::new()
            });
            row.push(if first {
                text(&p["tax_class"])
            } else {
                String::new()
            });
            row.push(text(&stock["on_hand"]));
            rows.push(row);
        }
    }
    Ok(csv_file(&format!("{shop}-products.csv"), &rows))
}

/// The shop's customers as CSV.
#[get("/admin/{shop}/transfer/customers.csv", tag = "admin")]
pub(crate) async fn export_customers(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let customers = page!(all(&cx, "/api/customers?sort=id").await);
    let mut rows = vec![CUSTOMER_COLUMNS.iter().map(|c| (*c).to_owned()).collect()];
    for c in customers {
        rows.push(vec![
            text(&c["email"]),
            text(&c["first_name"]),
            text(&c["last_name"]),
            text(&c["phone"]),
            if c["accepts_marketing"] == json!(true) {
                "yes"
            } else {
                "no"
            }
            .to_owned(),
            tags(&c["tags"]),
            text(&c["note"]),
        ]);
    }
    Ok(csv_file(&format!("{shop}-customers.csv"), &rows))
}

/// The uploaded file in field `file`, as text.
async fn upload(form: &mut Multipart) -> std::result::Result<String, String> {
    while let Some(field) = form.next_field().await.map_err(|e| e.to_string())? {
        if field.name() == Some("file") {
            let bytes = field.bytes().await.map_err(|e| e.to_string())?;
            return String::from_utf8(bytes.to_vec())
                .map_err(|_| "Save the file as CSV in UTF-8.".to_owned());
        }
    }
    Err("Choose a CSV file.".to_owned())
}

/// What an import did: counts and a message per row that failed.
#[derive(Default)]
pub(crate) struct Outcome {
    pub(crate) created: usize,
    pub(crate) updated: usize,
    pub(crate) problems: Vec<Value>,
    /// Images added from the file's links.
    pub(crate) images: usize,
    /// Links that could not be imported; their products were.
    pub(crate) image_problems: Vec<Value>,
}

impl Outcome {
    fn problem(&mut self, line: usize, message: impl Into<String>) {
        self.problems
            .push(json!({ "line": line, "message": message.into() }));
    }

    pub(crate) fn view(self, kind: &str) -> Value {
        json!({
            "kind": kind, "created": self.created, "updated": self.updated,
            "problems": self.problems, "failed": !self.problems.is_empty(),
            "images": self.images, "image_problems": self.image_problems,
        })
    }
}

/// Why a product was not imported: the API's status (422 for the file's
/// own problems) and a message.
type Fail = (u16, String);

fn bad(message: impl Into<String>) -> Fail {
    (422, message.into())
}

/// Row `r`'s line in the uploaded file: its `line` cell when the rows were
/// translated (Shopify's export), else after the header.
fn line_of(t: &Table, r: usize) -> usize {
    t.get(r, "line").parse().unwrap_or(r + 2)
}

fn reason(out: &Value) -> String {
    crate::util::message(out)
}

fn split_tags(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Import products from a CSV upload (`file`): rows with the same handle
/// are one product, a row per variant. A product whose handle the shop has
/// is updated, its variants matched by SKU; empty cells change nothing. A
/// `stock` cell sets the units on hand at the shop's first location.
#[post("/admin/{shop}/transfer/products", tag = "admin")]
pub(crate) async fn import_products(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    mut form: Multipart,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let table = match read(&mut form, "handle", |t| {
        if shopify::is_products(&t) {
            shopify::products(&t)
        } else if woocommerce::is_products(&t) {
            woocommerce::products(&t)
        } else if shopware::is_products(&t) {
            shopware::products(&t)
        } else {
            t
        }
    })
    .await
    {
        Ok(t) => t,
        Err(e) => return Ok(page(cx, None, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await),
    };
    let out = match import_product_rows(&cx, &table).await {
        Ok(out) => out,
        Err(stop) => return Ok(stop.into_response(&cx)),
    };
    let status = if out.problems.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    Ok(page(cx, Some(out.view("products")), None, status).await)
}

/// The uploaded CSV, turned into SOAT's file by `convert` when it is
/// another platform's export.
async fn read(
    form: &mut Multipart,
    key: &str,
    convert: impl FnOnce(Table) -> Table,
) -> std::result::Result<Table, String> {
    let text = upload(form).await?;
    let table = Table::parse(&text).map_err(|e| format!("The file is not valid CSV: {e}."))?;
    let table = convert(table);
    if table.column(key).is_none() {
        return Err(format!("The file needs a `{key}` column."));
    }
    if table.rows.len() > MAX_ROWS {
        return Err(format!("One file takes at most {MAX_ROWS} rows; split it."));
    }
    Ok(table)
}

/// One product from its rows: whether it was created, and its id.
async fn import_product(
    cx: &Ctx,
    t: &Table,
    handle: &str,
    rows: &[usize],
    profile: &Value,
    location: &mut Option<String>,
) -> std::result::Result<(bool, String), Fail> {
    let first = rows[0];
    let cell = |r: usize, c: &str| t.get(r, c).to_owned();
    let q = serde_urlencoded::to_string([("q", handle), ("limit", "200")]).unwrap_or_default();
    let (status, found) = cx
        .call(Method::GET, &format!("/api/products?{q}"), None)
        .await;
    if status != 200 {
        return Err((status, reason(&found)));
    }
    let existing = found["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["handle"] == handle)
        .cloned();
    let currency = match (&existing, cell(first, "currency")) {
        (Some(p), _) => text(&p["currency"]),
        (None, c) if !c.is_empty() => c.to_ascii_uppercase(),
        (None, _) => profile["currency"].as_str().unwrap_or("EUR").to_owned(),
    };
    let price = |r: usize| -> std::result::Result<Option<i64>, Fail> {
        match t.get(r, "price") {
            "" => Ok(None),
            p => amount(p, &currency)
                .map(Some)
                .ok_or_else(|| bad(format!("the price “{p}” is not an amount in {currency}"))),
        }
    };
    let names: Vec<String> = (1..=3)
        .map(|n| cell(first, &format!("option{n}_name")))
        .take_while(|n| !n.is_empty())
        .collect();
    let values = |r: usize| -> Vec<String> {
        (1..=names.len())
            .map(|n| cell(r, &format!("option{n}_value")))
            .collect()
    };
    let mut body = Map::new();
    for (column, key) in [
        ("title", "title"),
        ("description", "description"),
        ("status", "status"),
        ("tax_class", "tax_class"),
        ("seo_title", "seo_title"),
        ("seo_description", "seo_description"),
    ] {
        let v = cell(first, column);
        if !v.is_empty() {
            body.insert(key.into(), json!(v));
        }
    }
    let tags = cell(first, "tags");
    if !tags.is_empty() {
        body.insert("tags".into(), json!(split_tags(&tags)));
    }
    match cell(first, "weight_grams").as_str() {
        "" => {}
        w => {
            let g: i64 = w
                .parse()
                .map_err(|_| bad(format!("the weight “{w}” is not a number of grams")))?;
            body.insert("weight_grams".into(), json!(g));
        }
    }
    if let Some(p) = price(first)? {
        body.insert("price_minor".into(), json!(p));
    }
    let (created, id) = match &existing {
        Some(p) => {
            let id = text(&p["id"]);
            if !body.is_empty() {
                let (status, out) = cx
                    .call(
                        Method::PATCH,
                        &format!("/api/products/{id}"),
                        Some(&Value::Object(body)),
                    )
                    .await;
                if status != 200 {
                    return Err((status, reason(&out)));
                }
            }
            (false, id)
        }
        None => {
            if !body.contains_key("title") {
                return Err(bad("a new product needs a title"));
            }
            if !body.contains_key("price_minor") {
                return Err(bad("a new product needs a price"));
            }
            let sku = match cell(first, "sku") {
                s if !s.is_empty() && names.is_empty() => s,
                _ => handle.to_ascii_uppercase(),
            };
            body.insert("handle".into(), json!(handle));
            body.insert("sku".into(), json!(sku));
            body.insert("currency".into(), json!(currency));
            let (status, out) = cx
                .call(Method::POST, "/api/products", Some(&Value::Object(body)))
                .await;
            if status != 201 {
                return Err((status, reason(&out)));
            }
            (true, text(&out["id"]))
        }
    };
    // Stock per item: the product itself, or each variant.
    let mut stock: Vec<(String, String)> = Vec::new();
    if names.is_empty() {
        stock.push((id.clone(), cell(first, "stock")));
    } else {
        let (status, current) = cx
            .call(Method::GET, &format!("/api/products/{id}/options"), None)
            .await;
        if status != 200 {
            return Err((status, reason(&current)));
        }
        // Keep the values variants use now; add the file's.
        let mut options: Vec<(String, Vec<String>)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let kept = current
                    .as_array()
                    .and_then(|o| o.get(i))
                    .filter(|o| o["name"] == n.as_str())
                    .and_then(|o| o["values"].as_array())
                    .map(|v| v.iter().map(text).collect())
                    .unwrap_or_default();
                (n.clone(), kept)
            })
            .collect();
        for &r in rows {
            for (i, v) in values(r).into_iter().enumerate() {
                if v.is_empty() {
                    return Err(bad(format!(
                        "line {} has no value for {}",
                        line_of(t, r),
                        names[i]
                    )));
                }
                if !options[i].1.contains(&v) {
                    options[i].1.push(v);
                }
            }
        }
        let body = json!({ "options": options
            .iter()
            .map(|(n, v)| json!({ "name": n, "values": v }))
            .collect::<Vec<_>>() });
        let (status, out) = cx
            .call(
                Method::PUT,
                &format!("/api/products/{id}/options"),
                Some(&body),
            )
            .await;
        if status != 200 {
            return Err((status, reason(&out)));
        }
        let (_, variants) = cx
            .call(Method::GET, &format!("/api/products/{id}/variants"), None)
            .await;
        let variants = variants.as_array().cloned().unwrap_or_default();
        for &r in rows {
            let sku = cell(r, "sku");
            if sku.is_empty() {
                return Err(bad(format!(
                    "line {} has no SKU for its variant",
                    line_of(t, r)
                )));
            }
            let price = price(r).map_err(|(s, e)| {
                if r == first {
                    (s, e)
                } else {
                    (s, format!("line {}: {e}", line_of(t, r)))
                }
            })?;
            let same = variants.iter().find(|v| v["sku"] == sku.as_str());
            let vid = match same {
                Some(v) => {
                    // A variant's values stay; its price follows the file.
                    if let Some(p) = price.filter(|p| v["price_minor"] != json!(p)) {
                        let path =
                            format!("/api/products/{id}/variants/{}", encode(&text(&v["id"])));
                        let change = json!({ "price_minor": p });
                        let (status, out) = cx.call(Method::PATCH, &path, Some(&change)).await;
                        if status != 200 {
                            return Err((
                                status,
                                format!("line {}: {}", line_of(t, r), reason(&out)),
                            ));
                        }
                    }
                    text(&v["id"])
                }
                None => {
                    let mut new = json!({ "sku": sku, "options": values(r) });
                    if let Some(p) = price {
                        new["price_minor"] = json!(p);
                    }
                    let path = format!("/api/products/{id}/variants");
                    let (status, out) = cx.call(Method::POST, &path, Some(&new)).await;
                    if status != 201 {
                        return Err((status, format!("line {}: {}", line_of(t, r), reason(&out))));
                    }
                    text(&out["id"])
                }
            };
            stock.push((vid, cell(r, "stock")));
        }
    }
    for (item, units) in stock {
        if units.is_empty() {
            continue;
        }
        let n: i64 = units
            .parse()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| bad(format!("the stock “{units}” is not a number of units")))?;
        let loc = match location {
            Some(l) => l.clone(),
            None => {
                let (_, found) = cx.call(Method::GET, "/api/locations?limit=1", None).await;
                let l = match found["items"][0]["id"].as_str() {
                    Some(l) => l.to_owned(),
                    None => {
                        let country = profile["country"].as_str().unwrap_or("DE");
                        let body = json!({ "name": "Warehouse", "country": country });
                        let (status, out) =
                            cx.call(Method::POST, "/api/locations", Some(&body)).await;
                        if status != 201 {
                            return Err((status, reason(&out)));
                        }
                        text(&out["id"])
                    }
                };
                *location = Some(l.clone());
                l
            }
        };
        let body = json!({ "location_id": loc, "on_hand": n, "note": "CSV import" });
        let path = format!("/api/inventory/{}/count", encode(&item));
        let (status, out) = cx.call(Method::POST, &path, Some(&body)).await;
        if status != 200 {
            return Err((status, reason(&out)));
        }
    }
    Ok((created, id))
}

/// Adds the images the product's `images` cell links to; the API skips
/// links imported before. Problems are the links' own.
async fn import_images(cx: &Ctx, product: &str, cell: &str, line: usize, out: &mut Outcome) {
    let urls = soat_core::import::image_urls(cell);
    for chunk in urls.chunks(20) {
        let body = json!({ "urls": chunk });
        let path = format!("/api/products/{}/media/import", encode(product));
        let (status, got) = cx.call(Method::POST, &path, Some(&body)).await;
        if status != 200 {
            out.image_problems
                .push(json!({ "line": line, "message": reason(&got) }));
            continue;
        }
        out.images += got["imported"].as_u64().unwrap_or(0) as usize;
        for p in got["problems"].as_array().into_iter().flatten() {
            out.image_problems
                .push(json!({ "line": line, "message": text(&p["message"]) }));
        }
    }
}

fn yes(s: &str) -> Option<bool> {
    match s.to_lowercase().as_str() {
        "yes" | "true" | "1" | "y" | "ja" | "oui" => Some(true),
        "no" | "false" | "0" | "n" | "nein" | "non" => Some(false),
        _ => None,
    }
}

/// Import customers from a CSV upload (`file`): a customer whose email the
/// shop has is updated; empty cells change nothing.
#[post("/admin/{shop}/transfer/customers", tag = "admin")]
pub(crate) async fn import_customers(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    mut form: Multipart,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let table = match read(&mut form, "email", |t| {
        if shopify::is_customers(&t) {
            shopify::customers(&t)
        } else if shopware::is_customers(&t) {
            shopware::customers(&t)
        } else {
            t
        }
    })
    .await
    {
        Ok(t) => t,
        Err(e) => return Ok(page(cx, None, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await),
    };
    let out = match import_customer_rows(&cx, &table).await {
        Ok(out) => out,
        Err(stop) => return Ok(stop.into_response(&cx)),
    };
    let status = if out.problems.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    Ok(page(cx, Some(out.view("customers")), None, status).await)
}

/// Connect the shop's Shopify store (a form post with `shop_domain` and
/// `access_token`), to sync it until cutover.
#[post("/admin/{shop}/transfer/shopify", tag = "admin")]
pub(crate) async fn connect_shopify(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let f = Fields::parse(&body);
    let (status, out) = cx
        .call(
            Method::PUT,
            "/api/imports/shopify",
            Some(&json!({
                "shop_domain": f.get("shop_domain"),
                "access_token": f.get("access_token"),
            })),
        )
        .await;
    match status {
        200 => Ok(page(cx, None, None, StatusCode::OK).await),
        401 => Ok(crate::to_login()),
        403 => Ok(cx.failed(403, &out)),
        s => Ok(page(
            cx,
            None,
            Some(reason(&out)),
            StatusCode::from_u16(s).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY),
        )
        .await),
    }
}

/// Pull what changed on Shopify since the last sync and import it:
/// products, then customers. With `cutover` set, this is the last sync
/// and the shop cuts over when it went through.
#[post("/admin/{shop}/transfer/shopify/sync", tag = "admin")]
pub(crate) async fn sync_shopify(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let cutover = !Fields::parse(&body).get("cutover").is_empty();
    let mut total = Outcome::default();
    for what in ["products", "customers"] {
        // A pull takes up to 10 000; a few rounds catch up a big store.
        for _ in 0..5 {
            let (status, pulled) = cx
                .call(
                    Method::POST,
                    "/api/imports/shopify/pull",
                    Some(&json!({ "what": what })),
                )
                .await;
            match status {
                200 => {}
                401 => return Ok(crate::to_login()),
                403 => return Ok(cx.failed(403, &pulled)),
                s => {
                    let code = StatusCode::from_u16(s).unwrap_or(StatusCode::BAD_GATEWAY);
                    return Ok(page(cx, None, Some(reason(&pulled)), code).await);
                }
            }
            let table = Table::parse(pulled["csv"].as_str().unwrap_or_default()).unwrap_or(Table {
                headers: Vec::new(),
                rows: Vec::new(),
            });
            let out = match what {
                "products" => import_product_rows(&cx, &table).await,
                _ => import_customer_rows(&cx, &table).await,
            };
            let out = match out {
                Ok(out) => out,
                Err(stop) => return Ok(stop.into_response(&cx)),
            };
            total.created += out.created;
            total.updated += out.updated;
            if !out.problems.is_empty() {
                // Not recorded: the next sync pulls these again.
                total.problems.extend(out.problems);
                let result = Some(total.view(what));
                return Ok(page(cx, result, None, StatusCode::UNPROCESSABLE_ENTITY).await);
            }
            let (status, recorded) = cx
                .call(
                    Method::POST,
                    "/api/imports/shopify/synced",
                    Some(&json!({ "what": what, "until": pulled["until"] })),
                )
                .await;
            if status != 200 {
                let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
                return Ok(page(cx, None, Some(reason(&recorded)), code).await);
            }
            if pulled["more"] != true {
                break;
            }
        }
    }
    if cutover {
        let (status, out) = cx
            .call(Method::POST, "/api/imports/shopify/cutover", None)
            .await;
        if status != 200 {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            return Ok(page(cx, None, Some(reason(&out)), code).await);
        }
    }
    let kind = if cutover {
        "the last sync from Shopify"
    } else {
        "changes from Shopify"
    };
    Ok(page(cx, Some(total.view(kind)), None, StatusCode::OK).await)
}

/// Imports SOAT's product file through the API.
pub(crate) async fn import_product_rows(
    cx: &Ctx,
    table: &Table,
) -> std::result::Result<Outcome, Stop> {
    let (_, profile) = cx.call(Method::GET, "/api/shop", None).await;
    let mut out = Outcome::default();
    let mut location: Option<String> = None;
    // Rows by handle, in the order handles first appear.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for r in 0..table.rows.len() {
        let handle = table.get(r, "handle").to_lowercase();
        if handle.is_empty() {
            out.problem(line_of(table, r), "The handle is empty.");
            continue;
        }
        match groups.iter_mut().find(|(h, _)| *h == handle) {
            Some((_, rows)) => rows.push(r),
            None => groups.push((handle, vec![r])),
        }
    }
    for (handle, rows) in groups {
        let line = line_of(table, rows[0]);
        match import_product(cx, table, &handle, &rows, &profile, &mut location).await {
            Ok((created, id)) => {
                if created {
                    out.created += 1;
                } else {
                    out.updated += 1;
                }
                let images = table.get(rows[0], "images");
                if !images.is_empty() {
                    import_images(cx, &id, images, line, &mut out).await;
                }
            }
            Err((401, _)) => return Err(Stop::Login),
            Err((403, e)) => return Err(Stop::Forbidden(json!({ "error": e }))),
            Err((_, e)) => out.problem(line, format!("{handle}: {e}")),
        }
    }
    Ok(out)
}

/// Imports SOAT's customer file through the API.
pub(crate) async fn import_customer_rows(
    cx: &Ctx,
    table: &Table,
) -> std::result::Result<Outcome, Stop> {
    let mut out = Outcome::default();
    for r in 0..table.rows.len() {
        let line = r + 2;
        let email = table.get(r, "email").to_lowercase();
        if email.is_empty() {
            out.problem(line, "The email is empty.");
            continue;
        }
        let mut body = Map::new();
        for c in ["first_name", "last_name", "phone", "note"] {
            let v = table.get(r, c);
            if !v.is_empty() {
                body.insert(c.into(), json!(v));
            }
        }
        let tags = table.get(r, "tags");
        if !tags.is_empty() {
            body.insert("tags".into(), json!(split_tags(tags)));
        }
        match table.get(r, "accepts_marketing") {
            "" => {}
            v => match yes(v) {
                Some(b) => {
                    body.insert("accepts_marketing".into(), json!(b));
                }
                None => {
                    out.problem(
                        line,
                        format!("{email}: write yes or no for accepts_marketing"),
                    );
                    continue;
                }
            },
        }
        let q = serde_urlencoded::to_string([("q", email.as_str()), ("limit", "200")])
            .unwrap_or_default();
        let (_, found) = cx
            .call(Method::GET, &format!("/api/customers?{q}"), None)
            .await;
        let existing = found["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["email"].as_str().map(str::to_lowercase) == Some(email.clone()))
            .and_then(|c| c["id"].as_str().map(str::to_owned));
        let (status, res) = match existing {
            Some(id) => {
                if body.is_empty() {
                    out.updated += 1;
                    continue;
                }
                cx.call(
                    Method::PATCH,
                    &format!("/api/customers/{id}"),
                    Some(&Value::Object(body)),
                )
                .await
            }
            None => {
                body.insert("email".into(), json!(email));
                cx.call(Method::POST, "/api/customers", Some(&Value::Object(body)))
                    .await
            }
        };
        match status {
            200 => out.updated += 1,
            201 => out.created += 1,
            401 => return Err(Stop::Login),
            403 => return Err(Stop::Forbidden(res)),
            _ => out.problem(line, format!("{email}: {}", reason(&res))),
        }
    }
    Ok(out)
}

/// Why an import stopped before its end.
pub(crate) enum Stop {
    /// The session ended.
    Login,
    /// The staff member may not do this; the API's answer.
    Forbidden(Value),
}

impl Stop {
    pub(crate) fn into_response(self, cx: &Ctx) -> Response {
        match self {
            Self::Login => crate::to_login(),
            Self::Forbidden(res) => cx.failed(403, &res),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_read_as_yes_or_no() {
        assert_eq!(yes("Yes"), Some(true));
        assert_eq!(yes("0"), Some(false));
        assert_eq!(yes("maybe"), None);
    }

    #[test]
    fn tags_split_on_commas() {
        assert_eq!(split_tags(" a, b ,,c"), ["a", "b", "c"]);
    }
}
