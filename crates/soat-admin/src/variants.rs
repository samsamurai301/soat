//! A product's options, variants and media, on its page: options written
//! as lines (`Size: S, M, L`), variants to add, price or delete (or all
//! missing ones at once), and images to upload, describe, order and
//! remove. Every change goes through the API as the staff member.

use serde_json::{json, Value};
use soat_core::Currency;
use zapline::db::Db;
use zapline::multipart::Multipart;
use zapline::prelude::*;

use crate::settings::{amount, Fields};
use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// Options as the form writes them: `Name: value, value` per line.
pub(crate) fn options_text(options: &Value) -> String {
    options
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            let values: Vec<&str> = o["values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!(
                "{}: {}",
                o["name"].as_str().unwrap_or_default(),
                values.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The lines back as options, or what is wrong with them.
pub(crate) fn parse_options(text: &str) -> std::result::Result<Value, String> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some((name, values)) = line.split_once(':') else {
            return Err(format!(
                "Write “{line}” as a name, a colon and values: Size: S, M, L"
            ));
        };
        let values: Vec<&str> = values
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect();
        if name.trim().is_empty() || values.is_empty() {
            return Err(format!("“{line}” needs a name and at least one value."));
        }
        out.push(json!({ "name": name.trim(), "values": values }));
    }
    Ok(Value::Array(out))
}

/// Every combination of the options' values, in order.
pub(crate) fn combinations(options: &Value) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for o in options.as_array().into_iter().flatten() {
        let values: Vec<String> = o["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        out = out
            .into_iter()
            .flat_map(|c| {
                values.iter().map(move |v| {
                    let mut next = c.clone();
                    next.push(v.clone());
                    next
                })
            })
            .collect();
    }
    if out.len() == 1 && out[0].is_empty() {
        Vec::new()
    } else {
        out
    }
}

/// A SKU for a variant: the product's and its values, `TEE-S-RED`.
fn variant_sku(product: &str, values: &[String]) -> String {
    let mut sku = product.to_owned();
    for v in values {
        let part: String = v
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '-'
                }
            })
            .collect();
        let part = part.trim_matches('-');
        if !part.is_empty() {
            sku.push('-');
            sku.push_str(part);
        }
    }
    sku
}

/// What the product page shows besides the product: options, variants
/// with prices, and media.
pub(crate) async fn load(cx: &mut Ctx, product: &Value) {
    let id = encode(product["id"].as_str().unwrap_or_default());
    let currency = Currency::parse(product["currency"].as_str().unwrap_or_default()).ok();
    let (_, options) = cx
        .call(Method::GET, &format!("/api/products/{id}/options"), None)
        .await;
    let (_, variants) = cx
        .call(Method::GET, &format!("/api/products/{id}/variants"), None)
        .await;
    let (_, media) = cx
        .call(Method::GET, &format!("/api/products/{id}/media"), None)
        .await;
    let (_, downloads) = cx
        .call(Method::GET, &format!("/api/products/{id}/downloads"), None)
        .await;
    cx.set(
        "downloads",
        if downloads.is_array() {
            downloads
        } else {
            json!([])
        },
    );
    let options = if options.is_array() {
        options
    } else {
        json!([])
    };
    let variants: Vec<Value> = variants
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let price = currency
                .map(|c| c.decimal(v["price_minor"].as_i64().unwrap_or(0)))
                .unwrap_or_default();
            json!({
                "id": v["id"], "sku": v["sku"], "title": v["title"], "price": price,
                "options": v["options"],
            })
        })
        .collect();
    let have: Vec<Value> = variants.iter().map(|v| v["options"].clone()).collect();
    let missing = combinations(&options)
        .iter()
        .filter(|c| !have.contains(&json!(c)))
        .count();
    let media: Vec<Value> = media
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, m)| {
            let mut m = m.clone();
            m["image"] = json!(m["content_type"]
                .as_str()
                .is_some_and(|t| t.starts_with("image/")));
            m["first"] = json!(i == 0);
            m
        })
        .collect();
    let fields: Vec<Value> = options
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, o)| json!({ "name": o["name"], "field": format!("value{i}") }))
        .collect();
    cx.set("options_text", json!(options_text(&options)));
    cx.set("option_fields", Value::Array(fields));
    cx.set(
        "has_options",
        json!(options.as_array().is_some_and(|o| !o.is_empty())),
    );
    cx.set("variants", Value::Array(variants));
    cx.set("missing", json!(missing));
    cx.set("media", Value::Array(media));
}

/// The product page again, saying what went wrong.
async fn again(cx: Ctx, shop: &str, id: &str, error: String) -> Response {
    let (status, p) = cx
        .call(Method::GET, &format!("/api/products/{}", encode(id)), None)
        .await;
    if status != 200 {
        return cx.failed(status, &p);
    }
    let _ = shop;
    crate::products::page(cx, &p, Some(error), StatusCode::UNPROCESSABLE_ENTITY).await
}

fn back(shop: &str, id: &str, anchor: &str) -> Response {
    Redirect::to(&format!(
        "/admin/{shop}/products/{}?saved=1#{anchor}",
        encode(id)
    ))
    .into_response()
}

/// The API's answer as a page: back to the product, or the problem.
async fn answer(cx: Ctx, shop: &str, id: &str, anchor: &str, status: u16, out: &Value) -> Response {
    match status {
        200..=299 => back(shop, id, anchor),
        401 => crate::to_login(),
        403 | 404 => cx.failed(status, out),
        _ => again(cx, shop, id, message(out)).await,
    }
}

/// Set the product's options (a form post): a line per option.
#[post("/admin/{shop}/products/{id}/options", tag = "admin")]
pub(crate) async fn set_options(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let options = match parse_options(&Fields::parse(&body).get("options")) {
        Ok(o) => o,
        Err(e) => return Ok(again(cx, &shop, &id, e).await),
    };
    let path = format!("/api/products/{}/options", encode(&id));
    let (status, out) = cx
        .call(Method::PUT, &path, Some(&json!({ "options": options })))
        .await;
    Ok(answer(cx, &shop, &id, "variants", status, &out).await)
}

/// Add a variant (a form post): a value per option, a SKU and a price.
#[post("/admin/{shop}/products/{id}/variants", tag = "admin")]
pub(crate) async fn add_variant(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let f = Fields::parse(&body);
    let product = page!(cx.get(&format!("/api/products/{}", encode(&id))).await);
    let currency = product["currency"].as_str().unwrap_or_default().to_owned();
    let values: Vec<String> = (0..3)
        .map(|i| f.get(&format!("value{i}")))
        .filter(|v| !v.is_empty())
        .collect();
    let sku = match f.get("sku") {
        s if s.is_empty() => variant_sku(product["sku"].as_str().unwrap_or_default(), &values),
        s => s,
    };
    let mut body = json!({ "sku": sku, "options": values });
    let price = f.get("price");
    if !price.is_empty() {
        let Some(minor) = amount(&price, &currency) else {
            return Ok(again(
                cx,
                &shop,
                &id,
                format!("{price} is not a price in {currency}."),
            )
            .await);
        };
        body["price_minor"] = json!(minor);
    }
    let path = format!("/api/products/{}/variants", encode(&id));
    let (status, out) = cx.call(Method::POST, &path, Some(&body)).await;
    Ok(answer(cx, &shop, &id, "variants", status, &out).await)
}

/// Add every combination of option values that has no variant yet, at
/// the product's price (a form post).
#[post("/admin/{shop}/products/{id}/variants/all", tag = "admin")]
pub(crate) async fn add_all(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let product = page!(cx.get(&format!("/api/products/{}", encode(&id))).await);
    let options = page!(
        cx.get(&format!("/api/products/{}/options", encode(&id)))
            .await
    );
    let variants = page!(
        cx.get(&format!("/api/products/{}/variants", encode(&id)))
            .await
    );
    let have: Vec<Value> = variants
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| v["options"].clone())
        .collect();
    let base = product["sku"].as_str().unwrap_or_default();
    for values in combinations(&options) {
        if have.contains(&json!(values)) {
            continue;
        }
        let body = json!({ "sku": variant_sku(base, &values), "options": values });
        let path = format!("/api/products/{}/variants", encode(&id));
        let (status, out) = cx.call(Method::POST, &path, Some(&body)).await;
        if !(200..300).contains(&status) {
            return Ok(answer(cx, &shop, &id, "variants", status, &out).await);
        }
    }
    Ok(back(&shop, &id, "variants"))
}

/// Change a variant's SKU and price, or delete it (form posts).
#[post(
    "/admin/{shop}/products/{id}/variants/{variant}/{action}",
    tag = "admin"
)]
pub(crate) async fn change_variant(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, variant, action)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let path = format!(
        "/api/products/{}/variants/{}",
        encode(&id),
        encode(&variant)
    );
    let (status, out) = match action.as_str() {
        "delete" => cx.call(Method::DELETE, &path, None).await,
        "save" => {
            let f = Fields::parse(&body);
            let product = page!(cx.get(&format!("/api/products/{}", encode(&id))).await);
            let currency = product["currency"].as_str().unwrap_or_default().to_owned();
            let price = f.get("price");
            let Some(minor) = amount(&price, &currency) else {
                return Ok(again(
                    cx,
                    &shop,
                    &id,
                    format!("{price} is not a price in {currency}."),
                )
                .await);
            };
            let body = json!({ "sku": f.get("sku"), "price_minor": minor });
            cx.call(Method::PATCH, &path, Some(&body)).await
        }
        _ => return Ok(cx.failed(404, &Value::Null)),
    };
    Ok(answer(cx, &shop, &id, "variants", status, &out).await)
}

/// Upload an image or video to the product (a `multipart/form-data` form
/// post with `file` and `alt`); it goes last in the product's media.
#[post("/admin/{shop}/products/{id}/media", tag = "admin")]
pub(crate) async fn upload(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    form: Multipart,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    Ok(upload_into(cx, &shop, &id, form, "media").await)
}

/// Upload a file for buyers of a digital product to download (a
/// `multipart/form-data` form post with `file`); it goes last.
#[post("/admin/{shop}/products/{id}/downloads", tag = "admin")]
pub(crate) async fn upload_download(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    form: Multipart,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    Ok(upload_into(cx, &shop, &id, form, "downloads").await)
}

/// Take a file off a digital product's downloads (a form post).
#[post("/admin/{shop}/products/{id}/downloads/{file}/remove", tag = "admin")]
pub(crate) async fn remove_download(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, file)): Path<(String, String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let mut ids = file_ids(&cx, &id, "downloads").await;
    let Some(at) = ids.iter().position(|f| *f == file) else {
        return Ok(cx.failed(404, &Value::Null));
    };
    ids.remove(at);
    let (status, out) = put_files(&cx, &id, "downloads", &ids).await;
    Ok(answer(cx, &shop, &id, "downloads", status, &out).await)
}

/// Uploads the form's file and adds it last to the product's `list`
/// (`media` or `downloads`).
async fn upload_into(cx: Ctx, shop: &str, id: &str, mut form: Multipart, list: &str) -> Response {
    let mut file: Option<(String, Bytes)> = None;
    let mut alt = String::new();
    while let Ok(Some(field)) = form.next_field().await {
        match field.name() {
            Some("file") => {
                let name = field.file_name().unwrap_or("upload").to_owned();
                if let Ok(bytes) = field.bytes().await {
                    file = Some((name, bytes));
                }
            }
            Some("alt") => alt = field.text().await.unwrap_or_default(),
            _ => {}
        }
    }
    let Some((name, bytes)) = file.filter(|(_, b)| !b.is_empty()) else {
        return again(cx, shop, id, "Choose a file to upload.".into()).await;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let boundary = format!("----soat-upload-{nanos:x}-{:x}", bytes.len());
    let mut body = Vec::with_capacity(bytes.len() + 512);
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, '"' | '\r' | '\n' | '\\'))
        .collect();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\ncontent-disposition: form-data; name=\"alt\"\r\n\r\n{}\r\n\
             --{boundary}\r\ncontent-disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
             content-type: application/octet-stream\r\n\r\n",
            alt.trim()
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let (status, out) =
        cx.s.api
            .send_raw(
                &cx.db,
                Method::POST,
                "/api/files",
                Bytes::from(body),
                &format!("multipart/form-data; boundary={boundary}"),
                &cx.token,
                &cx.shop,
                cx.conn,
            )
            .await;
    if status != 201 {
        let e = match (status, list) {
            (415, "media") => {
                "Upload a PNG, JPEG, GIF, WebP or AVIF image, or an MP4 or WebM video.".into()
            }
            (415, _) => "Upload a PDF, EPUB, ZIP, MP3, MP4 or an image.".into(),
            _ => message(&out),
        };
        return match status {
            401 => crate::to_login(),
            403 => cx.failed(403, &out),
            _ => again(cx, shop, id, e).await,
        };
    }
    let mut ids = file_ids(&cx, id, list).await;
    ids.push(out["id"].as_str().unwrap_or_default().to_owned());
    let (status, out) = put_files(&cx, id, list, &ids).await;
    answer(cx, shop, id, list, status, &out).await
}

async fn file_ids(cx: &Ctx, id: &str, list: &str) -> Vec<String> {
    let (_, media) = cx
        .call(
            Method::GET,
            &format!("/api/products/{}/{list}", encode(id)),
            None,
        )
        .await;
    media
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect()
}

async fn put_files(cx: &Ctx, id: &str, list: &str, ids: &[String]) -> (u16, Value) {
    cx.call(
        Method::PUT,
        &format!("/api/products/{}/{list}", encode(id)),
        Some(&json!({ "file_ids": ids })),
    )
    .await
}

/// Move an image up or down, take it off the product, or change its alt
/// text (form posts).
#[post("/admin/{shop}/products/{id}/media/{file}/{action}", tag = "admin")]
pub(crate) async fn change_media(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, file, action)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    if action == "alt" {
        let alt = Fields::parse(&body).get("alt");
        let (status, out) = cx
            .call(
                Method::PATCH,
                &format!("/api/files/{}", encode(&file)),
                Some(&json!({ "alt": alt })),
            )
            .await;
        return Ok(answer(cx, &shop, &id, "media", status, &out).await);
    }
    let mut ids = file_ids(&cx, &id, "media").await;
    let Some(at) = ids.iter().position(|f| *f == file) else {
        return Ok(cx.failed(404, &Value::Null));
    };
    match action.as_str() {
        "up" if at > 0 => ids.swap(at, at - 1),
        "down" if at + 1 < ids.len() => ids.swap(at, at + 1),
        "remove" => {
            ids.remove(at);
        }
        "up" | "down" => return Ok(back(&shop, &id, "media")),
        _ => return Ok(cx.failed(404, &Value::Null)),
    }
    let (status, out) = put_files(&cx, &id, "media", &ids).await;
    Ok(answer(cx, &shop, &id, "media", status, &out).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip_and_combine() {
        let o = parse_options("Size: S, M\n\n Color : Red,Blue \n").unwrap();
        assert_eq!(
            o,
            json!([{ "name": "Size", "values": ["S", "M"] }, { "name": "Color", "values": ["Red", "Blue"] }])
        );
        assert_eq!(options_text(&o), "Size: S, M\nColor: Red, Blue");
        assert!(parse_options("Size S M").is_err());
        assert!(parse_options("Size:").is_err());
        assert_eq!(parse_options("").unwrap(), json!([]));
        let c = combinations(&o);
        assert_eq!(c.len(), 4);
        assert_eq!(c[1], vec!["S".to_owned(), "Blue".to_owned()]);
        assert!(combinations(&json!([])).is_empty());
        assert_eq!(variant_sku("TEE", &c[3]), "TEE-M-BLUE");
        assert_eq!(
            variant_sku("BEAN", &["Whole bean".into()]),
            "BEAN-WHOLE-BEAN"
        );
    }
}
