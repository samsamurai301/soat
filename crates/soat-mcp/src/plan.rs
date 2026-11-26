//! A shop from a description: Claude drafts products, collections and
//! pages for a shop its owner describes, as a plan to review. Nothing is
//! stored; whoever applies the plan does so through the API, with their
//! own permissions (the admin's shop builder does).

use serde_json::{json, Value};
use soat_tenancy::Tenant;
use zapline::db::Db;
use zapline::prelude::*;

use crate::chat::shop_key;
use crate::{claude, Shared, ASSISTANT_USE};

/// What the shop is about.
#[model]
pub struct PlanRequest {
    /// The shop in the owner's words: what it sells, to whom, its style.
    #[validate(length(min = 10, max = 4000))]
    pub description: String,
    /// The currency to price in, e.g. `EUR`.
    #[validate(length(equal = 3))]
    pub currency: String,
}

/// Most products, collections and pages a plan holds.
const MAX_PRODUCTS: usize = 24;
const MAX_COLLECTIONS: usize = 6;
const MAX_PAGES: usize = 6;

/// The plan's JSON Schema, for Claude's structured output.
fn schema() -> Value {
    let text = json!({ "type": "string" });
    let strings = json!({ "type": "array", "items": text });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "products", "collections", "pages"],
        "properties": {
            "summary": text,
            "products": { "type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "required": ["sku", "title", "description", "price", "tax_class",
                             "weight_grams", "tags", "option"],
                "properties": {
                    "sku": text, "title": text, "description": text,
                    "price": { "type": "string", "description": "A decimal amount, e.g. 12.90" },
                    "tax_class": { "type": "string", "enum": ["standard", "reduced", "exempt"] },
                    "weight_grams": { "type": "integer" },
                    "tags": strings,
                    "option": { "anyOf": [{ "type": "null" }, {
                        "type": "object", "additionalProperties": false,
                        "required": ["name", "values"],
                        "properties": { "name": text, "values": strings },
                    }] },
                },
            } },
            "collections": { "type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "required": ["title", "description", "skus"],
                "properties": { "title": text, "description": text, "skus": strings },
            } },
            "pages": { "type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "required": ["title", "handle", "body"],
                "properties": {
                    "title": text,
                    "handle": { "type": "string", "description": "lowercase-with-dashes" },
                    "body": { "type": "string", "description": "Markdown" },
                },
            } },
        },
    })
}

const BRIEF: &str = "You draft the start of an online shop on SOAT from its owner's description. \
Propose what the shop sells: products with a short SKU in capitals, a title, a two or three \
sentence description, a realistic price as a decimal in the shop's currency, the tax class \
(`reduced` only for goods that usually get a reduced VAT rate, such as food and books), a \
shipping weight in grams, a few lowercase tags, and at most one option (such as size or \
color) with its values when the product comes in variants. Group them in a few collections \
by SKU. Add pages a shop needs beyond its legal pages: About us, and shipping and returns \
information or an FAQ, in Markdown. Invent nothing about the owner that a customer could \
rely on (no addresses, awards or certifications); write [placeholders] where such facts go. \
Write everything in the language of the description. The summary says in one or two \
sentences what you drafted.";

/// Draft a shop from its description (`assistant:use`): products,
/// collections and pages, priced in `currency`, as a plan to review and
/// apply. Calls Claude with the shop's own Anthropic key (409
/// `no_assistant_key` without one); changes nothing.
#[post("/api/assistant/plan", tag = "assistant")]
async fn draft_plan(
    db: Db,
    tenant: Tenant,
    s: State<Shared>,
    Valid(Json(r)): Valid<Json<PlanRequest>>,
) -> Result<Json<Value>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let key = shop_key(&mut tx, &tenant, &s).await?;
    tx.commit().await?;
    let currency = r.currency.to_ascii_uppercase();
    let body = json!({
        "model": key.model,
        "max_tokens": 16_000,
        "system": BRIEF,
        "thinking": { "type": "adaptive" },
        "output_config": { "format": { "type": "json_schema", "schema": schema() } },
        "messages": [{ "role": "user", "content": format!(
            "The shop's currency: {currency}. At most {MAX_PRODUCTS} products, \
             {MAX_COLLECTIONS} collections and {MAX_PAGES} pages.\n\n<description>\n{}\n</description>",
            r.description.trim()
        ) }],
    });
    let out = claude::messages(&s.anthropic, &key, &body).await?;
    match out["stop_reason"].as_str().unwrap_or_default() {
        "refusal" => {
            return Err(Error::unprocessable(
                "Claude declined to draft this shop. Describe it another way.",
            )
            .with_code("refused"))
        }
        "max_tokens" => {
            return Err(Error::unprocessable(
                "The draft grew too long. Describe a smaller shop, or fewer products.",
            )
            .with_code("plan_too_long"))
        }
        _ => {}
    }
    let text: String = out["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    let plan: Value = serde_json::from_str(&text).map_err(|_| {
        Error::service_unavailable("Claude: the draft was not in the expected form; try again")
    })?;
    Ok(Json(tidy(plan)))
}

/// The plan within bounds: known keys only, lists cut to their maximum,
/// collections naming only SKUs the plan has.
pub(crate) fn tidy(plan: Value) -> Value {
    let list = |v: &Value, max: usize| -> Vec<Value> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter(|x| x.is_object())
                    .take(max)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let text = |v: &Value| json!(v.as_str().unwrap_or_default().trim());
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .take(20)
            .collect()
    };
    let mut skus: Vec<String> = Vec::new();
    let products: Vec<Value> = list(&plan["products"], MAX_PRODUCTS)
        .into_iter()
        .filter_map(|p| {
            let sku = p["sku"].as_str()?.trim().to_ascii_uppercase();
            if sku.is_empty() || skus.contains(&sku) {
                return None;
            }
            skus.push(sku.clone());
            let option = match strings(&p["option"]["values"]) {
                values if !values.is_empty() && p["option"]["name"].is_string() => {
                    json!({ "name": text(&p["option"]["name"]), "values": values })
                }
                _ => Value::Null,
            };
            let tax = ["reduced", "exempt"]
                .into_iter()
                .find(|c| p["tax_class"] == *c)
                .unwrap_or("standard");
            Some(json!({
                "sku": sku, "title": text(&p["title"]), "description": text(&p["description"]),
                "price": text(&p["price"]), "tax_class": tax,
                "weight_grams": p["weight_grams"].as_i64().unwrap_or(0).clamp(0, 1_000_000),
                "tags": strings(&p["tags"]), "option": option,
            }))
        })
        .collect();
    let collections: Vec<Value> = list(&plan["collections"], MAX_COLLECTIONS)
        .into_iter()
        .map(|c| {
            let skus: Vec<String> = strings(&c["skus"])
                .into_iter()
                .map(|s| s.to_ascii_uppercase())
                .filter(|s| skus.contains(s))
                .collect();
            json!({ "title": text(&c["title"]), "description": text(&c["description"]), "skus": skus })
        })
        .collect();
    let pages: Vec<Value> = list(&plan["pages"], MAX_PAGES)
        .into_iter()
        .map(|p| json!({ "title": text(&p["title"]), "handle": text(&p["handle"]), "body": text(&p["body"]) }))
        .collect();
    json!({
        "summary": text(&plan["summary"]),
        "products": products, "collections": collections, "pages": pages,
    })
}

pub(crate) fn mount(app: App) -> App {
    app.mount(draft_plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_stay_within_bounds() {
        let plan = tidy(json!({
            "summary": " A coffee shop. ",
            "products": [
                { "sku": "bean-1", "title": "Beans", "price": "12.90", "tax_class": "reduced",
                  "weight_grams": 250, "tags": ["beans", ""], "option": { "name": "Grind", "values": ["Whole", "Fine"] } },
                { "sku": "BEAN-1", "title": "Twice" },
                { "sku": "MUG", "title": "Mug", "price": "9", "tax_class": "luxury", "option": null },
                "not a product"
            ],
            "collections": [{ "title": "Coffee", "description": "", "skus": ["bean-1", "GHOST"] }],
            "pages": [{ "title": "About", "handle": "about", "body": "We roast." }],
            "extra": true
        }));
        assert_eq!(plan["summary"], "A coffee shop.");
        let products = plan["products"].as_array().unwrap();
        assert_eq!(products.len(), 2);
        assert_eq!(products[0]["sku"], "BEAN-1");
        assert_eq!(products[0]["tags"], json!(["beans"]));
        assert_eq!(products[0]["option"]["values"], json!(["Whole", "Fine"]));
        assert_eq!(products[1]["tax_class"], "standard");
        assert_eq!(products[1]["option"], Value::Null);
        assert_eq!(plan["collections"][0]["skus"], json!(["BEAN-1"]));
        assert!(plan.get("extra").is_none());
        assert_eq!(
            schema()["properties"]["products"]["items"]["additionalProperties"],
            false
        );
    }
}
