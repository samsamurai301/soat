//! Translations: the languages a shop publishes its storefront in, and the
//! translated texts of its products, collections, pages, menus and theme.
//!
//! A shop has a default language (its own texts are in it) and up to
//! [`MAX_LANGUAGES`] more. Each translatable resource keeps its fields per
//! language; a field without a translation shows the shop's own text.
//!
//! | resource type | fields |
//! |---|---|
//! | `product`, `collection` | `title`, `description`, `seo_title`, `seo_description` |
//! | `page` | `title`, `seo_title`, `seo_description`, `blocks` (JSON, as a page's blocks) |
//! | `menu` | `items` (JSON, as a menu's items) |
//! | `theme` (id `strings`) | any theme text key, e.g. `add_to_cart` |

use std::collections::HashMap;

use serde_json::Value;
use soat_core::content::{check_blocks, check_menu};
use soat_core::i18n::is_language_code;
use soat_core::Permission;
use soat_module::Module;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

/// See the shop's languages and translations.
pub const TRANSLATIONS_READ: Permission = Permission::new(
    "translations:read",
    "See the shop's languages and translations",
)
.for_staff();
/// Change the shop's languages and translations.
pub const TRANSLATIONS_WRITE: Permission = Permission::new(
    "translations:write",
    "Change the shop's languages and translations",
)
.for_staff();

/// Most languages besides the default.
pub const MAX_LANGUAGES: usize = 20;
/// Longest translated value, in characters.
pub const MAX_VALUE: usize = 50_000;
/// Most theme texts a shop translates per language.
pub const MAX_THEME_KEYS: usize = 500;
/// The default language of a shop that has not chosen one.
pub const FALLBACK_LANGUAGE: &str = "en";

/// The translations module.
pub struct I18n;

impl Module for I18n {
    fn name(&self) -> &'static str {
        "i18n"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        // Translated resources must exist: their tables come first.
        &["tenancy", "events", "catalog", "cms"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[TRANSLATIONS_READ, TRANSLATIONS_WRITE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            get_languages,
            put_languages,
            get_translations,
            put_translations,
            delete_translations,
        ))
    }
}

/// The languages a shop publishes its storefront in.
#[model]
pub struct ShopLanguages {
    /// The language of the shop's own texts, e.g. `en`; addresses carry no
    /// prefix for it.
    pub default_language: String,
    /// More languages, e.g. `["de", "fr"]`; their addresses start with
    /// `/de/`.
    pub languages: Vec<String>,
}

impl ShopLanguages {
    /// Every published language, the default first.
    pub fn all(&self) -> Vec<String> {
        let mut out = vec![self.default_language.clone()];
        out.extend(self.languages.iter().cloned());
        out
    }

    /// Whether `lang` is one of the extra languages.
    pub fn has(&self, lang: &str) -> bool {
        self.languages.iter().any(|l| l == lang)
    }
}

/// The shop's languages; `en` alone until it chooses. Run it in the
/// shop's transaction.
///
/// # Errors
///
/// Database errors.
pub async fn shop_languages(conn: &mut PgConnection, scope: &impl Scope) -> Result<ShopLanguages> {
    let row: Option<(String, Vec<String>)> = sqlx::query_as(
        "SELECT default_language, languages FROM shop_languages WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(conn)
    .await?;
    Ok(match row {
        Some((default_language, languages)) => ShopLanguages {
            default_language,
            languages,
        },
        None => ShopLanguages {
            default_language: FALLBACK_LANGUAGE.to_owned(),
            languages: Vec::new(),
        },
    })
}

/// The translated fields of resources `ids` of `resource_type` in `lang`:
/// id → field → value. Run it in the shop's transaction.
///
/// # Errors
///
/// Database errors.
pub async fn translated(
    conn: &mut PgConnection,
    scope: &impl Scope,
    resource_type: &str,
    ids: &[String],
    lang: &str,
) -> Result<HashMap<String, HashMap<String, String>>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT resource_id, field, value FROM translations \
         WHERE tenant_id = $1 AND resource_type = $2 AND language = $3 \
         AND resource_id = ANY($4)",
    )
    .bind(scope.tenant_id())
    .bind(resource_type)
    .bind(lang)
    .bind(ids)
    .fetch_all(conn)
    .await?;
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (id, field, value) in rows {
        out.entry(id).or_default().insert(field, value);
    }
    Ok(out)
}

/// The shop's own theme texts in `lang` (key → text).
///
/// # Errors
///
/// Database errors.
pub async fn theme_strings(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &str,
) -> Result<HashMap<String, String>> {
    let mut all = translated(conn, scope, "theme", &["strings".to_owned()], lang).await?;
    Ok(all.remove("strings").unwrap_or_default())
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_translation")
}

/// Checks a resource type and id, and returns the fields it allows (none
/// listed: any theme text key).
fn resource(resource_type: &str, id: &str) -> Result<&'static [&'static str]> {
    let (prefix, fields): (&str, &'static [&'static str]) = match resource_type {
        "product" => (
            "prod_",
            &["title", "description", "seo_title", "seo_description"],
        ),
        "collection" => (
            "col_",
            &["title", "description", "seo_title", "seo_description"],
        ),
        "page" => (
            "page_",
            &["title", "seo_title", "seo_description", "blocks"],
        ),
        "menu" => ("menu_", &["items"]),
        "theme" => {
            return if id == "strings" {
                Ok(&[])
            } else {
                Err(Error::not_found())
            }
        }
        _ => return Err(Error::not_found()),
    };
    let ok = id.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty() && rest.len() <= 64 && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    });
    if ok {
        Ok(fields)
    } else {
        Err(Error::not_found())
    }
}

/// A 404 unless the shop has the resource. Row-level security hides other
/// shops' rows, so their ids are not found either.
async fn exists(conn: &mut PgConnection, resource_type: &str, id: &str) -> Result<()> {
    let table = match resource_type {
        "product" => "products",
        "collection" => "collections",
        "page" => "pages",
        "menu" => "menus",
        _ => return Ok(()),
    };
    let found: Option<(i32,)> = sqlx::query_as(&format!("SELECT 1 FROM {table} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    found.map(|_| ()).ok_or_else(Error::not_found)
}

fn is_theme_key(k: &str) -> bool {
    (1..=64).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Checks one field's value.
fn check_field(resource_type: &str, allowed: &[&str], field: &str, value: &str) -> Result<()> {
    if allowed.is_empty() {
        if !is_theme_key(field) {
            return Err(invalid(format!(
                "{field} is not a theme text key (a-z, 0-9 and _)"
            )));
        }
    } else if !allowed.contains(&field) {
        return Err(invalid(format!(
            "a {resource_type} translates {}",
            allowed.join(", ")
        )));
    }
    if value.chars().count() > MAX_VALUE {
        return Err(invalid(format!(
            "{field} is longer than {MAX_VALUE} characters"
        )));
    }
    if field == "blocks" || field == "items" {
        let parsed: Vec<Value> =
            serde_json::from_str(value).map_err(|_| invalid(format!("{field} is a JSON list")))?;
        let checked = if field == "blocks" {
            check_blocks(&parsed)
        } else {
            check_menu(&parsed)
        };
        checked.map_err(invalid)?;
    }
    Ok(())
}

/// The shop's languages (`translations:read`).
#[get("/api/languages", tag = "translations")]
async fn get_languages(db: Db, tenant: Tenant) -> Result<Json<ShopLanguages>> {
    let mut tx = tenant.begin_for(&db, &TRANSLATIONS_READ).await?;
    let langs = shop_languages(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(langs))
}

/// Set the shop's default language and the others it publishes
/// (`translations:write`), as codes like `de` or `de-at`. Translations of a
/// language that is dropped stay, for when it comes back. Publishes
/// `languages.updated`.
#[put("/api/languages", tag = "translations")]
async fn put_languages(
    db: Db,
    tenant: Tenant,
    Json(mut l): Json<ShopLanguages>,
) -> Result<Json<ShopLanguages>> {
    let bad = |m: &str| Error::unprocessable(m.to_owned()).with_code("invalid_languages");
    l.default_language = l.default_language.trim().to_ascii_lowercase();
    if !is_language_code(&l.default_language) {
        return Err(bad("default_language is a code like en or de-at"));
    }
    let mut langs: Vec<String> = Vec::new();
    for code in &l.languages {
        let code = code.trim().to_ascii_lowercase();
        if !is_language_code(&code) {
            return Err(bad(&format!("{code} is not a code like de or de-at")));
        }
        if code != l.default_language && !langs.contains(&code) {
            langs.push(code);
        }
    }
    if langs.len() > MAX_LANGUAGES {
        return Err(bad(&format!("at most {MAX_LANGUAGES} languages")));
    }
    l.languages = langs;
    let mut tx = tenant.begin_for(&db, &TRANSLATIONS_WRITE).await?;
    sqlx::query(
        "INSERT INTO shop_languages (tenant_id, default_language, languages) VALUES ($1, $2, $3) \
         ON CONFLICT (tenant_id) DO UPDATE SET default_language = EXCLUDED.default_language, \
         languages = EXCLUDED.languages, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(&l.default_language)
    .bind(&l.languages)
    .execute(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "languages.updated", &l).await?;
    tx.commit().await?;
    Ok(Json(l))
}

/// One translated field.
#[model]
#[derive(sqlx::FromRow)]
pub struct Translation {
    pub language: String,
    pub field: String,
    pub value: String,
    /// RFC 3339, UTC.
    pub updated_at: String,
}

/// A resource's translations, every language.
#[model]
pub struct Translations {
    pub items: Vec<Translation>,
}

/// Body of a translation change: field → text. An empty text removes the
/// field's translation.
#[model]
pub struct TranslationChange {
    pub fields: HashMap<String, String>,
}

const COLUMNS: &str = "language, field, value, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

/// A resource's translations in every language (`translations:read`).
/// Types: `product`, `collection`, `page`, `menu`, and `theme` with id
/// `strings`.
#[get("/api/translations/{resource_type}/{id}", tag = "translations")]
async fn get_translations(
    db: Db,
    tenant: Tenant,
    Path((resource_type, id)): Path<(String, String)>,
) -> Result<Json<Translations>> {
    resource(&resource_type, &id)?;
    let mut tx = tenant.begin_for(&db, &TRANSLATIONS_READ).await?;
    exists(&mut tx, &resource_type, &id).await?;
    let items = sqlx::query_as::<_, Translation>(&format!(
        "SELECT {COLUMNS} FROM translations WHERE tenant_id = $1 AND resource_type = $2 \
         AND resource_id = $3 ORDER BY language, field"
    ))
    .bind(tenant.id())
    .bind(&resource_type)
    .bind(&id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(Translations { items }))
}

/// Translate fields of a resource into one of the shop's languages
/// (`translations:write`); an empty text removes a field's translation.
/// A field the resource does not translate, a language the shop does not
/// publish, or a bad page block or menu item is a 422
/// `invalid_translation`. Publishes `translation.updated` with the
/// fields' names.
#[put(
    "/api/translations/{resource_type}/{id}/{language}",
    tag = "translations"
)]
async fn put_translations(
    db: Db,
    tenant: Tenant,
    Path((resource_type, id, language)): Path<(String, String, String)>,
    Json(change): Json<TranslationChange>,
) -> Result<Json<Translations>> {
    let allowed = resource(&resource_type, &id)?;
    for (field, value) in &change.fields {
        if !value.is_empty() {
            check_field(&resource_type, allowed, field, value)?;
        } else if !allowed.is_empty() && !allowed.contains(&field.as_str()) {
            return Err(invalid(format!(
                "a {resource_type} does not translate {field}"
            )));
        }
    }
    let mut tx = tenant.begin_for(&db, &TRANSLATIONS_WRITE).await?;
    exists(&mut tx, &resource_type, &id).await?;
    let langs = shop_languages(&mut tx, &tenant).await?;
    if !langs.has(&language) {
        return Err(invalid(format!(
            "{language} is not one of the shop's other languages"
        )));
    }
    let mut fields: Vec<&String> = change.fields.keys().collect();
    fields.sort();
    for field in &fields {
        let value = &change.fields[*field];
        if value.is_empty() {
            sqlx::query(
                "DELETE FROM translations WHERE tenant_id = $1 AND resource_type = $2 \
                 AND resource_id = $3 AND language = $4 AND field = $5",
            )
            .bind(tenant.id())
            .bind(&resource_type)
            .bind(&id)
            .bind(&language)
            .bind(field)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO translations (tenant_id, resource_type, resource_id, language, \
                 field, value) VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, resource_type, resource_id, language, field) \
                 DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
            )
            .bind(tenant.id())
            .bind(&resource_type)
            .bind(&id)
            .bind(&language)
            .bind(field)
            .bind(value)
            .execute(&mut *tx)
            .await?;
        }
    }
    if resource_type == "theme" {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM translations WHERE tenant_id = $1 AND resource_type = 'theme' \
             AND language = $2",
        )
        .bind(tenant.id())
        .bind(&language)
        .fetch_one(&mut *tx)
        .await?;
        if count > MAX_THEME_KEYS as i64 {
            return Err(invalid(format!(
                "at most {MAX_THEME_KEYS} theme texts per language"
            )));
        }
    }
    let items = sqlx::query_as::<_, Translation>(&format!(
        "SELECT {COLUMNS} FROM translations WHERE tenant_id = $1 AND resource_type = $2 \
         AND resource_id = $3 AND language = $4 ORDER BY field"
    ))
    .bind(tenant.id())
    .bind(&resource_type)
    .bind(&id)
    .bind(&language)
    .fetch_all(&mut *tx)
    .await?;
    tenant
        .publish(
            &mut tx,
            "translation.updated",
            &serde_json::json!({
                "resource_type": resource_type, "resource_id": id,
                "language": language, "fields": fields,
            }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(Translations { items }))
}

/// Remove a resource's translations into one language
/// (`translations:write`). Publishes `translation.deleted`.
#[delete(
    "/api/translations/{resource_type}/{id}/{language}",
    tag = "translations",
    status = 204
)]
async fn delete_translations(
    db: Db,
    tenant: Tenant,
    Path((resource_type, id, language)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    resource(&resource_type, &id)?;
    let mut tx = tenant.begin_for(&db, &TRANSLATIONS_WRITE).await?;
    exists(&mut tx, &resource_type, &id).await?;
    sqlx::query(
        "DELETE FROM translations WHERE tenant_id = $1 AND resource_type = $2 \
         AND resource_id = $3 AND language = $4",
    )
    .bind(tenant.id())
    .bind(&resource_type)
    .bind(&id)
    .bind(&language)
    .execute(&mut *tx)
    .await?;
    tenant
        .publish(
            &mut tx,
            "translation.deleted",
            &serde_json::json!({
                "resource_type": resource_type, "resource_id": id, "language": language,
            }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_and_fields() {
        assert!(resource("product", "prod_01abc").is_ok());
        assert!(resource("product", "col_01abc").is_err());
        assert!(resource("product", "prod_").is_err());
        assert!(resource("product", "prod_a/b").is_err());
        assert!(resource("order", "ord_1").is_err());
        assert!(resource("theme", "strings").unwrap().is_empty());
        assert!(resource("theme", "other").is_err());
        let page = resource("page", "page_1").unwrap();
        assert!(check_field("page", page, "title", "Über uns").is_ok());
        assert!(check_field("page", page, "handle", "x").is_err());
        assert!(check_field(
            "page",
            page,
            "blocks",
            "[{\"type\":\"heading\",\"text\":\"Hallo\"}]"
        )
        .is_ok());
        assert!(check_field("page", page, "blocks", "[{\"type\":\"script\"}]").is_err());
        assert!(check_field("page", page, "blocks", "nope").is_err());
        assert!(check_field("theme", &[], "add_to_cart", "In den Warenkorb").is_ok());
        assert!(check_field("theme", &[], "Add To Cart", "x").is_err());
    }
}
