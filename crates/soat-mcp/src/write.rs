//! Content helpers: Claude writes a product's or collection's copy and
//! search-engine fields, or translates them, as suggestions. Nothing is
//! saved; staff review a suggestion and save it themselves.

use serde_json::{json, Value};
use soat_tenancy::Tenant;
use zapline::db::Db;
use zapline::prelude::*;

use crate::chat::shop_key;
use crate::{claude, Shared, ASSISTANT_USE};

/// What to write.
#[model]
pub struct WriteRequest {
    /// `product` or `collection`.
    pub resource: String,
    /// The product's or collection's id.
    pub id: String,
    /// `copy`: a better description and search-engine title and
    /// description; `translate`: all four fields in `language`.
    pub task: String,
    /// For `translate`: a language code like `de` or `fr-be`.
    pub language: Option<String>,
    /// What the staff member wants, in their words.
    #[validate(length(max = 1000))]
    pub notes: Option<String>,
}

/// Claude's suggestion.
#[model]
pub struct Suggestion {
    pub title: String,
    /// Markdown.
    pub description: String,
    pub seo_title: String,
    pub seo_description: String,
}

fn schema() -> Value {
    let text = json!({ "type": "string" });
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["title", "description", "seo_title", "seo_description"],
        "properties": {
            "title": text, "description": text, "seo_title": text, "seo_description": text,
        },
    })
}

const BRIEF: &str = "You write for an online shop on SOAT. You get one product or collection \
as JSON and a task. `copy`: keep the title as it is; write a description in Markdown of 60 to \
150 words that helps a buyer decide (what it is, what it is for, what makes it good, practical \
details), an SEO title of at most 60 characters and an SEO description of at most 155 \
characters, in the language the existing text is written in. `translate`: translate the \
title, description, SEO title and SEO description into the language named, keeping Markdown, \
product names and units; write what is missing from the description. Never invent facts a \
buyer could rely on (materials, certifications, origins, sizes) that the data does not \
give; leave them out. Follow the staff member's notes where they are given.";

/// Have Claude write copy for a product or collection, or translate it
/// (`assistant:use`, plus reading the resource). Answers suggested
/// fields; saves nothing. Calls Claude with the shop's own key (409
/// `no_assistant_key` without one).
#[post("/api/assistant/write", tag = "assistant")]
async fn write(
    db: Db,
    tenant: Tenant,
    s: State<Shared>,
    headers: HeaderMap,
    conn: ConnInfo,
    Valid(Json(r)): Valid<Json<WriteRequest>>,
) -> Result<Json<Suggestion>> {
    if !r.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::not_found());
    }
    let path = match (r.resource.as_str(), r.id.as_str()) {
        ("product", id) if id.starts_with("prod_") => format!("/api/products/{id}"),
        ("collection", id) if id.starts_with("col_") => format!("/api/collections/{id}"),
        _ => {
            return Err(
                Error::unprocessable("resource is a product or a collection, with its id")
                    .with_code("invalid_resource"),
            )
        }
    };
    let task = match (r.task.as_str(), r.language.as_deref().map(str::trim)) {
        ("copy", _) => "copy".to_owned(),
        ("translate", Some(l))
            if !l.is_empty()
                && l.len() <= 10
                && l.chars().all(|c| c.is_ascii_alphabetic() || c == '-') =>
        {
            format!("translate into the language with the code `{l}`")
        }
        _ => {
            return Err(Error::unprocessable(
                "task is copy, or translate with a language code like de",
            )
            .with_code("invalid_task"))
        }
    };
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let key = shop_key(&mut tx, &tenant, &s).await?;
    tx.commit().await?;
    let (status, item) = s
        .api
        .send(&db, Method::GET, &path, None, &headers, conn)
        .await;
    match status {
        200 => {}
        403 => return Err(Error::forbidden()),
        _ => return Err(Error::not_found()),
    }
    let item = json!({
        "title": item["title"], "description": item["description"],
        "seo_title": item["seo_title"], "seo_description": item["seo_description"],
        "tags": item["tags"], "options": item["options"],
        "price_minor": item["price_minor"], "currency": item["currency"],
        "weight_grams": item["weight_grams"],
    });
    let notes = r.notes.as_deref().unwrap_or("").trim();
    let body = json!({
        "model": key.model,
        "max_tokens": 8000,
        "system": BRIEF,
        "thinking": { "type": "adaptive" },
        "output_config": { "format": { "type": "json_schema", "schema": schema() } },
        "messages": [{ "role": "user", "content": format!(
            "Task: {task}.\n\n<{res}>\n{item}\n</{res}>\n\n<notes>\n{notes}\n</notes>",
            res = r.resource,
        ) }],
    });
    let out = claude::messages(&s.anthropic, &key, &body).await?;
    if out["stop_reason"] == "refusal" {
        return Err(Error::unprocessable("Claude declined to write this.").with_code("refused"));
    }
    let text: String = out["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        Error::service_unavailable("Claude: the answer was not in the expected form; try again")
    })?;
    let field = |name: &str, max: usize| -> String {
        v[name]
            .as_str()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(max)
            .collect()
    };
    Ok(Json(Suggestion {
        title: field("title", 500),
        description: field("description", 20_000),
        seo_title: field("seo_title", 200),
        seo_description: field("seo_description", 500),
    }))
}

pub(crate) fn mount(app: App) -> App {
    app.mount(write)
}
