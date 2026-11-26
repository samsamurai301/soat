//! Sample data: store templates a new shop loads to see itself working,
//! and removes again in one go.

use serde_json::{json, Value};
use zapline::prelude::*;

use crate::settings::amount;
use crate::util::encode;
use crate::Ctx;

/// The tag every sample product carries; removing samples deletes them.
pub(crate) const TAG: &str = "sample";

/// Units of each sample product (or variant) put in stock.
const STOCK: i64 = 25;

/// The store templates, one JSON file each.
const SOURCES: [&str; 3] = [
    include_str!("../samples/apparel.json"),
    include_str!("../samples/coffee.json"),
    include_str!("../samples/home.json"),
];

fn templates() -> Vec<Value> {
    SOURCES
        .iter()
        .map(|s| serde_json::from_str(s).expect("sample templates are valid JSON"))
        .collect()
}

/// The templates to offer: key, name and summary.
pub(crate) fn offered() -> Vec<Value> {
    templates()
        .iter()
        .map(|t| json!({ "key": t["key"], "name": t["name"], "summary": t["summary"] }))
        .collect()
}

/// What went wrong: the API's status and body.
pub(crate) type Failed = (u16, Value);

fn refused(message: &str) -> Failed {
    (422, json!({ "error": message }))
}

pub(crate) async fn call(
    cx: &Ctx,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, Failed> {
    let (status, out) = cx.call(method, path, body).await;
    if (200..300).contains(&status) {
        Ok(out)
    } else {
        Err((status, out))
    }
}

/// Part of a SKU made from an option value: `Whole bean` is `WHOLE-BEAN`.
fn sku_part(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Load template `key` priced in `currency`: its products (tagged
/// [`TAG`], with variants and stock) in one collection, and a location to
/// hold the stock when the shop has none. A SKU the shop already uses is
/// skipped.
pub(crate) async fn load(
    cx: &Ctx,
    key: &str,
    currency: &str,
    country: &str,
) -> Result<usize, Failed> {
    let Some(t) = templates().into_iter().find(|t| t["key"] == key) else {
        return Err(refused("Choose one of the store templates."));
    };
    let existing = call(
        cx,
        Method::GET,
        &format!("/api/products?tag={TAG}&limit=1"),
        None,
    )
    .await?;
    if existing["total"].as_i64().unwrap_or(0) > 0 {
        return Err(refused(
            "Sample products are loaded already. Remove them first.",
        ));
    }
    let location = location(cx, country).await?;
    let c = &t["collection"];
    let body = json!({
        "title": c["title"], "description": c["description"], "handle": format!("{TAG}-{key}"),
    });
    let collection = match call(cx, Method::POST, "/api/collections", Some(&body)).await {
        Ok(c) => c["id"].as_str().unwrap_or_default().to_owned(),
        // Left from samples removed before; reuse it.
        Err((409, _)) => {
            let path = format!("/api/collections?q={TAG}-{key}");
            let found = call(cx, Method::GET, &path, None).await?;
            found["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|c| c["handle"] == format!("{TAG}-{key}"))
                .and_then(|c| c["id"].as_str())
                .unwrap_or_default()
                .to_owned()
        }
        Err(e) => return Err(e),
    };
    let mut loaded = 0;
    for p in t["products"].as_array().into_iter().flatten() {
        let Some(id) = create_product(cx, p, currency, Some(TAG), Some((&location, STOCK))).await?
        else {
            continue;
        };
        let path = format!("/api/collections/{collection}/products/{id}");
        call(cx, Method::PUT, &path, None).await?;
        loaded += 1;
    }
    Ok(loaded)
}

/// The shop's first stock location, made (`Warehouse` in `country`) when
/// it has none.
pub(crate) async fn location(cx: &Ctx, country: &str) -> Result<String, Failed> {
    let locations = call(cx, Method::GET, "/api/locations?limit=1", None).await?;
    Ok(match locations["items"][0]["id"].as_str() {
        Some(id) => id.to_owned(),
        None => {
            let body = json!({ "name": "Warehouse", "country": country });
            let l = call(cx, Method::POST, "/api/locations", Some(&body)).await?;
            l["id"].as_str().unwrap_or_default().to_owned()
        }
    })
}

/// Creates product `p` as a store template writes it (`sku`, `title`,
/// `description`, decimal `price`, `tax_class`, `weight_grams`, `tags`,
/// one `option` with `values`), active and priced in `currency`, with a
/// variant per option value, an extra `tag`, and `stock` units of each at
/// a location. `None` when the shop has its SKU already.
pub(crate) async fn create_product(
    cx: &Ctx,
    p: &Value,
    currency: &str,
    tag: Option<&str>,
    stock: Option<(&str, i64)>,
) -> Result<Option<String>, Failed> {
    let sku = p["sku"].as_str().unwrap_or_default();
    let Some(price) = amount(p["price"].as_str().unwrap_or_default(), currency) else {
        return Err(refused(&format!(
            "{} is not a price in {currency}.",
            p["price"].as_str().unwrap_or_default()
        )));
    };
    let mut tags = p["tags"].as_array().cloned().unwrap_or_default();
    if let Some(t) = tag {
        tags.push(json!(t));
    }
    let body = json!({
        "sku": sku, "title": p["title"], "description": p["description"],
        "price_minor": price, "currency": currency, "tags": tags, "status": "active",
        "tax_class": p["tax_class"].as_str().unwrap_or("standard"),
        "weight_grams": p["weight_grams"],
    });
    let product = match call(cx, Method::POST, "/api/products", Some(&body)).await {
        Ok(p) => p,
        Err((409, _)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let id = product["id"].as_str().unwrap_or_default().to_owned();
    let mut stocked = vec![id.clone()];
    if let Some(values) = p["option"]["values"].as_array() {
        let options = json!({ "options": [{ "name": p["option"]["name"], "values": values }] });
        call(
            cx,
            Method::PUT,
            &format!("/api/products/{id}/options"),
            Some(&options),
        )
        .await?;
        stocked.clear();
        for v in values.iter().filter_map(Value::as_str) {
            let body = json!({ "sku": format!("{sku}-{}", sku_part(v)), "options": [v] });
            match call(
                cx,
                Method::POST,
                &format!("/api/products/{id}/variants"),
                Some(&body),
            )
            .await
            {
                Ok(variant) => stocked.push(variant["id"].as_str().unwrap_or_default().to_owned()),
                Err((409, _)) => {}
                Err(e) => return Err(e),
            }
        }
    }
    if let Some((location, units)) = stock {
        for item in stocked {
            let body = json!({ "location_id": location, "delta": units, "reason": "received" });
            let path = format!("/api/inventory/{}/adjustments", encode(&item));
            call(cx, Method::POST, &path, Some(&body)).await?;
        }
    }
    Ok(Some(id))
}

/// Delete every sample product and the templates' collections.
pub(crate) async fn remove(cx: &Ctx) -> Result<(), Failed> {
    loop {
        let page = call(
            cx,
            Method::GET,
            &format!("/api/products?tag={TAG}&limit=200"),
            None,
        )
        .await?;
        let ids: Vec<String> = page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        if ids.is_empty() {
            break;
        }
        for id in ids {
            call(cx, Method::DELETE, &format!("/api/products/{id}"), None).await?;
        }
    }
    for t in templates() {
        let handle = format!("{TAG}-{}", t["key"].as_str().unwrap_or_default());
        let found = call(
            cx,
            Method::GET,
            &format!("/api/collections?q={handle}"),
            None,
        )
        .await?;
        for c in found["items"].as_array().into_iter().flatten() {
            if c["handle"] == handle.as_str() {
                let id = c["id"].as_str().unwrap_or_default();
                call(cx, Method::DELETE, &format!("/api/collections/{id}"), None).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soat_core::Currency;

    #[test]
    fn templates_are_complete() {
        let eur = Currency::parse("EUR").unwrap();
        for t in templates() {
            assert!(t["key"].is_string() && t["name"].is_string() && t["summary"].is_string());
            assert!(t["collection"]["title"].is_string());
            let products = t["products"].as_array().unwrap();
            assert!(products.len() >= 3, "{}", t["key"]);
            for p in products {
                assert!(
                    eur.parse_decimal(p["price"].as_str().unwrap()).is_some(),
                    "{p}"
                );
                assert!(p["sku"].as_str().unwrap().len() <= 40);
            }
        }
        assert_eq!(offered().len(), SOURCES.len());
    }

    #[test]
    fn option_values_become_sku_parts() {
        assert_eq!(sku_part("Whole bean"), "WHOLE-BEAN");
        assert_eq!(sku_part("XL"), "XL");
        assert_eq!(sku_part(" Chalk / white "), "CHALK-WHITE");
    }
}
