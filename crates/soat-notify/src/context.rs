//! What templates can read: the shop, the order with amounts written out,
//! and the shipment or refund an email is about.

use serde_json::{json, Value};
use soat_core::Currency;
use soat_orders::{Order, PostalAddress};

/// An address as lines (name, company, street, town, country) and as text.
fn address(a: &PostalAddress) -> Value {
    let name = format!("{} {}", a.first_name, a.last_name)
        .trim()
        .to_owned();
    let town = format!("{} {}", a.postal_code, a.city).trim().to_owned();
    let lines: Vec<String> = [
        name,
        a.company.clone(),
        a.line1.clone(),
        a.line2.clone(),
        town,
        a.region.clone(),
        a.country.clone(),
    ]
    .into_iter()
    .filter(|l| !l.trim().is_empty())
    .collect();
    let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
    v["text"] = Value::String(lines.join("\n"));
    v["lines"] = json!(lines);
    v
}

/// The order as templates see it: amounts like `19.99 EUR`.
pub fn order(o: &Order) -> Value {
    let currency = Currency::parse(&o.currency).ok();
    let money = |minor: i64| match currency {
        Some(c) => c.format(minor),
        None => format!("{minor} {}", o.currency),
    };
    let lines: Vec<Value> = o
        .lines
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "title": l.title,
                "sku": l.sku,
                "quantity": l.quantity,
                "unit_price": money(l.unit_price_minor),
                "total": money(l.total_minor - l.discount_minor),
            })
        })
        .collect();
    json!({
        "id": o.id,
        "number": o.number,
        "email": o.email,
        "currency": o.currency,
        "lines": lines,
        "subtotal": money(o.subtotal_minor),
        "shipping": money(o.shipping_minor),
        "shipping_name": o.shipping_name.clone().unwrap_or_default(),
        "tax": money(o.tax_minor),
        "discount": money(o.discount_minor),
        "discount_code": o.discount_code,
        "total": money(o.total_minor),
        "prices_include_tax": o.prices_include_tax,
        "shipping_address": o.shipping_address.as_ref().map(address),
        "billing_address": o.billing_address.as_ref().map(address),
        "cancel_reason": o.cancel_reason,
        "status": o.status,
        "financial_status": o.financial_status,
    })
}

/// A shipment from its event payload, with the lines' titles.
pub fn fulfillment(o: &Order, f: &Value) -> Value {
    let lines: Vec<Value> = f["lines"]
        .as_array()
        .map(|ls| {
            ls.iter()
                .map(|l| {
                    let line = o
                        .lines
                        .iter()
                        .find(|ol| Some(ol.id.as_str()) == l["line_id"].as_str());
                    json!({
                        "title": line.map(|ol| ol.title.clone()).unwrap_or_default(),
                        "sku": line.map(|ol| ol.sku.clone()).unwrap_or_default(),
                        "quantity": l["quantity"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": f["id"],
        "carrier": f["carrier"],
        "tracking_number": f["tracking_number"],
        "tracking_url": f["tracking_url"],
        "lines": lines,
    })
}

/// A refund from its event payload.
pub fn refund(o: &Order, r: &Value) -> Value {
    let amount = r["amount_minor"].as_i64().unwrap_or(0);
    let amount = match Currency::parse(&o.currency) {
        Ok(c) => c.format(amount),
        Err(_) => format!("{amount} {}", o.currency),
    };
    json!({ "amount": amount, "reason": r["reason"] })
}

/// Made-up data for previews.
pub fn sample(shop: &str) -> Value {
    json!({
        "shop": { "name": shop },
        "customer": { "first_name": "Ada", "email": "ada@example.com" },
        "access": {
            "link": "https://shop.example.com/account/access?token=sample",
            "code": "sample",
            "minutes": 60,
        },
        "gift_card": { "code": "ABCD-EFGH-JKLM-NPQR", "amount": "50.00 EUR", "expires_at": null },
        "order": {
            "id": "ord_sample",
            "number": 1001,
            "email": "ada@example.com",
            "currency": "EUR",
            "lines": [
                { "id": "oli_1", "title": "Linen shirt", "sku": "SHIRT-M", "quantity": 2, "unit_price": "39.00 EUR", "total": "78.00 EUR" },
                { "id": "oli_2", "title": "Canvas tote", "sku": "TOTE", "quantity": 1, "unit_price": "19.00 EUR", "total": "19.00 EUR" }
            ],
            "subtotal": "97.00 EUR",
            "shipping": "4.95 EUR",
            "shipping_name": "Standard",
            "tax": "16.28 EUR",
            "discount": "0.00 EUR",
            "discount_code": null,
            "total": "101.95 EUR",
            "prices_include_tax": true,
            "shipping_address": {
                "text": "Ada Lovelace\nHauptstr. 1\n10115 Berlin\nDE",
                "lines": ["Ada Lovelace", "Hauptstr. 1", "10115 Berlin", "DE"]
            },
            "cancel_reason": "Out of stock",
            "status": "open",
            "financial_status": "paid"
        },
        "fulfillment": {
            "carrier": "DHL",
            "tracking_number": "00340434161094042557",
            "tracking_url": "https://www.dhl.de/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=00340434161094042557",
            "lines": [{ "title": "Linen shirt", "sku": "SHIRT-M", "quantity": 2 }]
        },
        "refund": { "amount": "19.00 EUR", "reason": "Returned tote" }
    })
}
