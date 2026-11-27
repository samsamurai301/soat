//! Themes: how the storefront looks.
//!
//! A theme is a set of files: page templates (`index`, `product`,
//! `collection`, `search`, `page`, `cart`, `checkout`, `order`, `login`,
//! `register`, `account`, `not_found`),
//! the `layout` that wraps every page (it writes the page with
//! `{{> content }}`), partials
//! (`header`, `footer`, `product_card`, `product_list`, `blocks`, and any
//! `snippet-…` a shop adds) and `theme.css`. Templates use the safe
//! language of [`soat_core::template`]: they read the values a page is
//! given and nothing else, so a shop can edit them freely.
//!
//! SOAT ships a default theme (D2). A shop changes any file; its version
//! replaces the default until it is reset. Theme settings (accent colour,
//! announcement bar, language) need no template edits.

use std::collections::BTreeMap;

use serde_json::Value;
use soat_core::template::Template;
use soat_core::Permission;
use soat_module::Module;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

/// See the theme's files and settings.
pub const THEME_READ: Permission = Permission::new(
    "theme:read",
    "See the storefront theme's files and settings",
)
.for_staff();
/// Change the theme's files and settings: what every buyer sees.
pub const THEME_WRITE: Permission = Permission::new(
    "theme:write",
    "Change the storefront theme's files and settings",
);

/// The default theme's files: name, what it is, source.
pub const DEFAULT_FILES: &[(&str, Kind, &str)] = &[
    ("layout", Kind::Layout, include_str!("../theme/layout.html")),
    ("index", Kind::Page, include_str!("../theme/index.html")),
    ("product", Kind::Page, include_str!("../theme/product.html")),
    (
        "collection",
        Kind::Page,
        include_str!("../theme/collection.html"),
    ),
    ("search", Kind::Page, include_str!("../theme/search.html")),
    ("page", Kind::Page, include_str!("../theme/page.html")),
    ("blog", Kind::Page, include_str!("../theme/blog.html")),
    ("post", Kind::Page, include_str!("../theme/post.html")),
    ("cart", Kind::Page, include_str!("../theme/cart.html")),
    (
        "checkout",
        Kind::Page,
        include_str!("../theme/checkout.html"),
    ),
    ("order", Kind::Page, include_str!("../theme/order.html")),
    ("login", Kind::Page, include_str!("../theme/login.html")),
    (
        "register",
        Kind::Page,
        include_str!("../theme/register.html"),
    ),
    ("account", Kind::Page, include_str!("../theme/account.html")),
    (
        "wishlist",
        Kind::Page,
        include_str!("../theme/wishlist.html"),
    ),
    ("loyalty", Kind::Page, include_str!("../theme/loyalty.html")),
    (
        "unsubscribe",
        Kind::Page,
        include_str!("../theme/unsubscribe.html"),
    ),
    ("forgot", Kind::Page, include_str!("../theme/forgot.html")),
    ("access", Kind::Page, include_str!("../theme/access.html")),
    (
        "not_found",
        Kind::Page,
        include_str!("../theme/not_found.html"),
    ),
    (
        "header",
        Kind::Partial,
        include_str!("../theme/header.html"),
    ),
    (
        "footer",
        Kind::Partial,
        include_str!("../theme/footer.html"),
    ),
    (
        "product_card",
        Kind::Partial,
        include_str!("../theme/product_card.html"),
    ),
    (
        "product_list",
        Kind::Partial,
        include_str!("../theme/product_list.html"),
    ),
    (
        "blocks",
        Kind::Partial,
        include_str!("../theme/blocks.html"),
    ),
    ("theme.css", Kind::Style, include_str!("../theme/theme.css")),
];

/// The default theme's texts, by language. Others fall back to English.
pub const LOCALES: &[(&str, &str)] = &[
    ("en", include_str!("../theme/locales/en.json")),
    ("de", include_str!("../theme/locales/de.json")),
];

/// The default theme's texts in `lang` (templates read them as
/// `{{ t.add_to_cart }}`): English, then the base language (`de` for
/// `de-at`), then `lang` itself.
pub fn strings(lang: &str) -> serde_json::Map<String, Value> {
    let parse = |code: &str| {
        LOCALES
            .iter()
            .find(|(c, _)| *c == code)
            .and_then(|(_, src)| serde_json::from_str::<serde_json::Map<String, Value>>(src).ok())
            .unwrap_or_default()
    };
    let mut out = parse("en");
    let base = lang.split('-').next().unwrap_or_default();
    for code in [base, lang] {
        if code != "en" {
            out.extend(parse(code));
        }
    }
    out
}

/// Prefix of the partials a shop adds.
pub const SNIPPET_PREFIX: &str = "snippet-";
/// Most snippets a shop may add.
pub const MAX_SNIPPETS: i64 = 50;
/// Longest stylesheet, in bytes.
pub const MAX_CSS_BYTES: usize = 256 * 1024;

/// What a theme file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Layout,
    Page,
    Partial,
    Style,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Layout => "layout",
            Kind::Page => "page",
            Kind::Partial => "partial",
            Kind::Style => "style",
        }
    }
}

fn kind_of(name: &str) -> Option<Kind> {
    DEFAULT_FILES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, k, _)| *k)
        .or_else(|| is_snippet(name).then_some(Kind::Partial))
}

fn is_snippet(name: &str) -> bool {
    name.strip_prefix(SNIPPET_PREFIX).is_some_and(|rest| {
        (1..=48).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    })
}

fn default_source(name: &str) -> Option<&'static str> {
    DEFAULT_FILES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, _, s)| *s)
}

/// Checks a file before it is saved.
fn check(name: &str, kind: Kind, source: &str) -> std::result::Result<(), String> {
    if kind == Kind::Style {
        if source.len() > MAX_CSS_BYTES {
            return Err(format!("a stylesheet is at most {MAX_CSS_BYTES} bytes"));
        }
        if source.to_ascii_lowercase().contains("</style") {
            return Err("a stylesheet cannot contain </style".to_owned());
        }
        return Ok(());
    }
    let t = Template::parse(source).map_err(|e| format!("{name}: {e}"))?;
    if kind == Kind::Layout && !t.partials().contains(&"content") {
        return Err("the layout must write the page with {{> content }}".to_owned());
    }
    Ok(())
}

/// A shop's theme, ready to render.
pub struct Theme {
    templates: BTreeMap<String, Template>,
    css: String,
    /// The shop's settings, as templates see them under `theme`.
    pub settings: ThemeSettings,
}

impl Theme {
    /// The default theme with default settings.
    pub fn default_theme() -> Self {
        Self::with_overrides(Vec::new(), ThemeSettings::default())
    }

    fn with_overrides(overrides: Vec<(String, String)>, settings: ThemeSettings) -> Self {
        let mut templates = BTreeMap::new();
        let mut css = default_source("theme.css").unwrap_or_default().to_owned();
        for (name, kind, source) in DEFAULT_FILES {
            if *kind != Kind::Style {
                if let Ok(t) = Template::parse(source) {
                    templates.insert((*name).to_owned(), t);
                }
            }
        }
        for (name, source) in overrides {
            match kind_of(&name) {
                Some(Kind::Style) => css = source,
                Some(_) => {
                    // Saved files were checked; a file that no longer parses
                    // (a newer template language) keeps the default.
                    if let Ok(t) = Template::parse(&source) {
                        templates.insert(name, t);
                    }
                }
                None => {}
            }
        }
        Self {
            templates,
            css,
            settings,
        }
    }

    /// The stylesheet.
    pub fn css(&self) -> &str {
        &self.css
    }

    /// Renders page template `page` inside the layout. `data` gains
    /// `theme` (the settings), and `t` (the theme's texts in the page's
    /// language) unless it has them. An unknown page renders `not_found`.
    pub fn render(&self, page: &str, data: &Value) -> String {
        let mut data = data.clone();
        if let Value::Object(map) = &mut data {
            map.insert(
                "theme".to_owned(),
                serde_json::to_value(&self.settings).unwrap_or_default(),
            );
            if !map.contains_key("shop") {
                map.insert("shop".to_owned(), serde_json::json!({}));
            }
            if let Some(Value::Object(shop)) = map.get_mut("shop") {
                shop.entry("language").or_insert_with(|| {
                    Value::String(
                        self.settings
                            .language
                            .clone()
                            .unwrap_or_else(|| "en".into()),
                    )
                });
            }
            if !map.contains_key("t") {
                let lang = map["shop"]["language"].as_str().unwrap_or("en").to_owned();
                map.insert("t".to_owned(), Value::Object(strings(&lang)));
            }
        }
        let content = self
            .templates
            .get(page)
            .or_else(|| self.templates.get("not_found"));
        let partials = |name: &str| -> Option<&Template> {
            if name == "content" {
                content
            } else {
                self.templates.get(name)
            }
        };
        match self.templates.get("layout") {
            Some(layout) => layout.render_with(&data, true, &partials),
            None => String::new(),
        }
    }
}

/// Settings every theme reads under `theme`.
#[model]
#[derive(Default)]
pub struct ThemeSettings {
    /// A CSS colour: `#rgb` or `#rrggbb`.
    pub accent_color: Option<String>,
    /// A line above the header, e.g. `Free shipping over €50`.
    #[validate(length(max = 200))]
    pub announcement: Option<String>,
    /// The pages' language, e.g. `en` or `de-AT`.
    pub language: Option<String>,
}

fn is_hex_color(s: &str) -> bool {
    s.strip_prefix('#')
        .is_some_and(|h| (h.len() == 3 || h.len() == 6) && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_language(s: &str) -> bool {
    (2..=12).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The shop's theme: its files over the defaults, and its settings.
pub async fn load(conn: &mut PgConnection, scope: &impl Scope) -> Result<Theme> {
    let files: Vec<(String, String)> =
        sqlx::query_as("SELECT name, source FROM theme_files WHERE tenant_id = $1")
            .bind(scope.tenant_id())
            .fetch_all(&mut *conn)
            .await?;
    let settings = settings(conn, scope).await?;
    Ok(Theme::with_overrides(files, settings))
}

async fn settings(conn: &mut PgConnection, scope: &impl Scope) -> Result<ThemeSettings> {
    let row: Option<(SqlJson<ThemeSettings>,)> =
        sqlx::query_as("SELECT settings FROM theme_settings WHERE tenant_id = $1")
            .bind(scope.tenant_id())
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|(s,)| s.0).unwrap_or_default())
}

/// The theme module.
pub struct Themes;

impl Module for Themes {
    fn name(&self) -> &'static str {
        "theme"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[THEME_READ, THEME_WRITE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_files,
            get_file,
            put_file,
            reset_file,
            get_settings,
            put_settings,
        ))
    }
}

/// A theme file in the list.
#[model]
pub struct ThemeFileSummary {
    pub name: String,
    /// `layout`, `page`, `partial` or `style`.
    pub kind: String,
    /// Whether the shop changed it.
    pub customized: bool,
}

/// A theme file.
#[model]
pub struct ThemeFile {
    pub name: String,
    pub kind: String,
    /// What the storefront uses.
    pub source: String,
    /// The default theme's version; null for a snippet.
    pub default_source: Option<String>,
    pub customized: bool,
}

/// A file's new source.
#[model]
pub struct FileInput {
    pub source: String,
}

async fn own(conn: &mut PgConnection, scope: &impl Scope, name: &str) -> Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT source FROM theme_files WHERE tenant_id = $1 AND name = $2")
            .bind(scope.tenant_id())
            .bind(name)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|(s,)| s))
}

/// The theme's files: the default theme's, and the shop's snippets
/// (`theme:read`).
#[get("/api/theme/files", tag = "theme")]
async fn list_files(db: Db, tenant: Tenant) -> Result<Json<Vec<ThemeFileSummary>>> {
    let mut tx = tenant.begin_for(&db, &THEME_READ).await?;
    let names: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM theme_files WHERE tenant_id = $1 ORDER BY name")
            .bind(tenant.id())
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    let names: Vec<String> = names.into_iter().map(|(n,)| n).collect();
    let mut out: Vec<ThemeFileSummary> = DEFAULT_FILES
        .iter()
        .map(|(name, kind, _)| ThemeFileSummary {
            name: (*name).to_owned(),
            kind: kind.as_str().to_owned(),
            customized: names.iter().any(|n| n == name),
        })
        .collect();
    out.extend(
        names
            .iter()
            .filter(|n| is_snippet(n))
            .map(|n| ThemeFileSummary {
                name: n.clone(),
                kind: "partial".to_owned(),
                customized: true,
            }),
    );
    Ok(Json(out))
}

/// One theme file, as used and as shipped (`theme:read`).
#[get("/api/theme/files/{name}", tag = "theme")]
async fn get_file(db: Db, tenant: Tenant, Path(name): Path<String>) -> Result<Json<ThemeFile>> {
    let kind = kind_of(&name).ok_or_else(Error::not_found)?;
    let mut tx = tenant.begin_for(&db, &THEME_READ).await?;
    let mine = own(&mut tx, &tenant, &name).await?;
    tx.commit().await?;
    let default = default_source(&name).map(str::to_owned);
    let source = mine
        .clone()
        .or_else(|| default.clone())
        .ok_or_else(Error::not_found)?;
    Ok(Json(ThemeFile {
        name,
        kind: kind.as_str().to_owned(),
        source,
        default_source: default,
        customized: mine.is_some(),
    }))
}

/// Change a theme file, or add a snippet (`snippet-…`) (`theme:write`).
/// A template that does not parse, a layout without `{{> content }}` or a
/// bad stylesheet is a 422 (`invalid_theme_file`). Publishes
/// `theme_file.updated`.
#[put("/api/theme/files/{name}", tag = "theme")]
async fn put_file(
    db: Db,
    tenant: Tenant,
    Path(name): Path<String>,
    Json(input): Json<FileInput>,
) -> Result<Json<ThemeFile>> {
    let kind = kind_of(&name).ok_or_else(|| Error::not_found().with_code("unknown_theme_file"))?;
    check(&name, kind, &input.source)
        .map_err(|e| Error::unprocessable(e).with_code("invalid_theme_file"))?;
    let mut tx = tenant.begin_for(&db, &THEME_WRITE).await?;
    if is_snippet(&name) && own(&mut tx, &tenant, &name).await?.is_none() {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM theme_files WHERE tenant_id = $1 AND name LIKE 'snippet-%'",
        )
        .bind(tenant.id())
        .fetch_one(&mut *tx)
        .await?;
        if n >= MAX_SNIPPETS {
            return Err(Error::unprocessable(format!(
                "a theme has at most {MAX_SNIPPETS} snippets"
            ))
            .with_code("too_many_snippets"));
        }
    }
    sqlx::query(
        "INSERT INTO theme_files (tenant_id, name, source) VALUES ($1, $2, $3) \
         ON CONFLICT (tenant_id, name) DO UPDATE SET source = EXCLUDED.source, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(&name)
    .bind(&input.source)
    .execute(&mut *tx)
    .await?;
    tenant
        .publish(
            &mut tx,
            "theme_file.updated",
            &serde_json::json!({ "name": name, "bytes": input.source.len() }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(ThemeFile {
        kind: kind.as_str().to_owned(),
        default_source: default_source(&name).map(str::to_owned),
        name,
        source: input.source,
        customized: true,
    }))
}

/// Go back to the default theme's version of a file, or delete a snippet
/// (`theme:write`). Publishes `theme_file.reset`.
#[delete("/api/theme/files/{name}", tag = "theme", status = 204)]
async fn reset_file(db: Db, tenant: Tenant, Path(name): Path<String>) -> Result<StatusCode> {
    kind_of(&name).ok_or_else(Error::not_found)?;
    let mut tx = tenant.begin_for(&db, &THEME_WRITE).await?;
    let gone = sqlx::query("DELETE FROM theme_files WHERE tenant_id = $1 AND name = $2")
        .bind(tenant.id())
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() > 0 {
        tenant
            .publish(
                &mut tx,
                "theme_file.reset",
                &serde_json::json!({ "name": name }),
            )
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The theme's settings (`theme:read`).
#[get("/api/theme/settings", tag = "theme")]
async fn get_settings(db: Db, tenant: Tenant) -> Result<Json<ThemeSettings>> {
    let mut tx = tenant.begin_for(&db, &THEME_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Set the theme's settings (`theme:write`). Publishes
/// `theme_settings.updated`.
#[put("/api/theme/settings", tag = "theme")]
async fn put_settings(
    db: Db,
    tenant: Tenant,
    Valid(Json(s)): Valid<Json<ThemeSettings>>,
) -> Result<Json<ThemeSettings>> {
    if s.accent_color.as_deref().is_some_and(|c| !is_hex_color(c)) {
        return Err(Error::unprocessable("accent_color is #rgb or #rrggbb")
            .with_code("invalid_theme_settings"));
    }
    if s.language.as_deref().is_some_and(|l| !is_language(l)) {
        return Err(Error::unprocessable("language is a code like en or de-AT")
            .with_code("invalid_theme_settings"));
    }
    let mut tx = tenant.begin_for(&db, &THEME_WRITE).await?;
    sqlx::query(
        "INSERT INTO theme_settings (tenant_id, settings) VALUES ($1, $2) \
         ON CONFLICT (tenant_id) DO UPDATE SET settings = EXCLUDED.settings, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(SqlJson(&s))
    .execute(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "theme_settings.updated", &s)
        .await?;
    tx.commit().await?;
    Ok(Json(s))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_language_has_every_text() {
        let en = strings("en");
        assert!(en.len() > 100);
        for (code, _) in LOCALES {
            let s = strings(code);
            assert_eq!(s.len(), en.len(), "{code}");
        }
        assert_eq!(strings("de-at")["add_to_cart"], "In den Warenkorb");
        assert_eq!(strings("xx")["add_to_cart"], "Add to cart");
    }

    #[test]
    fn the_default_theme_parses() {
        for (name, kind, source) in DEFAULT_FILES {
            check(name, *kind, source).unwrap();
        }
    }

    #[test]
    fn pages_render_in_the_layout() {
        let t = Theme::default_theme();
        let html = t.render(
            "product",
            &json!({
                "page_title": "Trail <Shoe>",
                "shop": { "name": "Acme" },
                "menus": { "main": { "items": [{ "title": "Shoes", "url": "/collections/shoes" }] } },
                "product": { "id": "prod_1", "title": "Trail <Shoe>", "price": "€129.99",
                             "description": "Light and *grippy*.", "images": [], "variants": [] }
            }),
        );
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<title>Trail &lt;Shoe&gt; · Acme</title>"));
        assert!(html.contains("<h1>Trail &lt;Shoe&gt;</h1>"));
        assert!(html.contains("<em>grippy</em>"));
        assert!(html.contains("href=\"/collections/shoes\""));
        assert!(html.contains("lang=\"en\""));
        assert!(t.render("nope", &json!({})).contains("Not found"));
    }

    #[test]
    fn files_are_checked() {
        assert!(check("index", Kind::Page, "{{#if x}}").is_err());
        assert_eq!(
            check("layout", Kind::Layout, "<body></body>").unwrap_err(),
            "the layout must write the page with {{> content }}"
        );
        assert!(check("theme.css", Kind::Style, "a{} </STYLE><script>").is_err());
        assert!(is_snippet("snippet-trust-badges"));
        assert!(!is_snippet("snippet-"));
        assert!(!is_snippet("snippet-A"));
        assert_eq!(kind_of("header"), Some(Kind::Partial));
        assert_eq!(kind_of("other"), None);
        assert!(is_hex_color("#1f6feb") && is_hex_color("#fff") && !is_hex_color("red"));
    }

    #[test]
    fn a_shops_files_replace_the_defaults() {
        let t = Theme::with_overrides(
            vec![
                ("not_found".into(), "<p>Gone fishing</p>".into()),
                ("theme.css".into(), "body{}".into()),
                ("index".into(), "{{#if".into()),
            ],
            ThemeSettings {
                accent_color: Some("#ff0000".into()),
                ..Default::default()
            },
        );
        let html = t.render("not_found", &json!({}));
        assert!(html.contains("Gone fishing"));
        assert!(html.contains("--accent: #ff0000"));
        assert_eq!(t.css(), "body{}");
        // A broken file keeps the default.
        assert!(t
            .render("index", &json!({ "shop": { "name": "Acme" } }))
            .contains("Shop all"));
    }
}
