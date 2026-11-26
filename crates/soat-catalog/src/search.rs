//! Storefront search with facets: full-text search over titles, SKUs, tags
//! and descriptions, filtered by tags, option values (`Size`, `Color`),
//! price and collection, with counts for each filter value.
//!
//! Each facet counts the products that match every other filter, so
//! choosing a value in one facet never hides the other values of that
//! facet.

use soat_tenancy::{Scope, Shop};
use zapline::db::sqlx::postgres::PgArguments;
use zapline::db::sqlx::query::QueryAs;
use zapline::db::sqlx::{PgConnection, Postgres};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::storage::Storage;

use crate::storefront::{first_images, ProductCard};

/// Longest search text.
pub const MAX_QUERY: usize = 200;
/// Most values a buyer can choose in one facet.
pub const MAX_VALUES: usize = 20;
/// Most option names to filter by at once.
pub const MAX_OPTIONS: usize = 10;
/// Most values a facet lists.
pub const FACET_VALUES: i64 = 30;

/// How results are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// Best match first (the default with a search text).
    Relevance,
    /// Newest first (the default without one).
    Newest,
    PriceLow,
    PriceHigh,
    Title,
}

impl Sort {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "relevance" => Self::Relevance,
            "newest" => Self::Newest,
            "price" => Self::PriceLow,
            "-price" => Self::PriceHigh,
            "title" => Self::Title,
            _ => return None,
        })
    }

    /// The value of `?sort=`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Newest => "newest",
            Self::PriceLow => "price",
            Self::PriceHigh => "-price",
            Self::Title => "title",
        }
    }
}

/// A search, from a query string: `q`, `tag` (repeatable),
/// `option.<Name>` (repeatable, e.g. `option.Size=42`), `min_price` and
/// `max_price` (minor units), `collection` (a handle), `sort`
/// (`relevance`, `newest`, `price`, `-price`, `title`), `offset`, `limit`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchQuery {
    pub q: String,
    pub tags: Vec<String>,
    /// Option name and the values any of which a product has.
    pub options: Vec<(String, Vec<String>)>,
    pub min_price: Option<i64>,
    pub max_price: Option<i64>,
    pub collection: Option<String>,
    pub sort: Option<Sort>,
    pub offset: i64,
    pub limit: i64,
}

impl SearchQuery {
    /// Reads a query string; what does not fit is left out.
    pub fn parse(query: &str) -> Self {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_str(query).unwrap_or_default();
        let mut s = SearchQuery {
            limit: 24,
            ..Self::default()
        };
        let clean = |v: &str| v.trim().chars().take(100).collect::<String>();
        for (k, v) in pairs {
            let v = v.trim();
            match k.as_str() {
                "q" => s.q = v.chars().take(MAX_QUERY).collect(),
                "tag" if !v.is_empty() && s.tags.len() < MAX_VALUES => {
                    let v = clean(v);
                    if !s.tags.contains(&v) {
                        s.tags.push(v);
                    }
                }
                "min_price" => s.min_price = v.parse().ok().filter(|p| *p >= 0),
                "max_price" => s.max_price = v.parse().ok().filter(|p| *p >= 0),
                "collection" if !v.is_empty() => s.collection = Some(clean(v)),
                "sort" => s.sort = Sort::parse(v),
                "offset" => s.offset = v.parse().unwrap_or(0).clamp(0, 100_000),
                "limit" => s.limit = v.parse().unwrap_or(24).clamp(1, 100),
                _ => {
                    let Some(name) = k.strip_prefix("option.") else {
                        continue;
                    };
                    let name = clean(name);
                    if name.is_empty() || v.is_empty() {
                        continue;
                    }
                    let v = clean(v);
                    let full = s.options.len() >= MAX_OPTIONS;
                    match s
                        .options
                        .iter_mut()
                        .find(|(n, _)| n.eq_ignore_ascii_case(&name))
                    {
                        Some((_, values)) => {
                            if values.len() < MAX_VALUES && !values.contains(&v) {
                                values.push(v);
                            }
                        }
                        None if !full => s.options.push((name, vec![v])),
                        None => {}
                    }
                }
            }
        }
        s
    }

    /// The sort in effect.
    pub fn sort(&self) -> Sort {
        self.sort.unwrap_or(if tsquery(&self.q).is_some() {
            Sort::Relevance
        } else {
            Sort::Newest
        })
    }
}

/// The text as a prefix match of every word, e.g. `trail:* & sho:*`; `None`
/// when it has no words.
fn tsquery(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(10)
        .map(|w| format!("{}:*", w.to_lowercase()))
        .collect();
    (!words.is_empty()).then(|| words.join(" & "))
}

/// One value of a facet.
#[model]
pub struct FacetValue {
    pub value: String,
    /// Products with it, among those matching the other filters.
    pub count: i64,
    pub selected: bool,
}

/// An option's values (`Size`: `42`, `43`).
#[model]
pub struct OptionFacet {
    pub name: String,
    pub values: Vec<FacetValue>,
}

/// The prices among the products matching the other filters.
#[model]
pub struct PriceRange {
    pub min_minor: i64,
    pub max_minor: i64,
    pub currency: String,
}

/// The filters buyers can choose from.
#[model]
pub struct Facets {
    pub tags: Vec<FacetValue>,
    pub options: Vec<OptionFacet>,
    pub price: Option<PriceRange>,
}

/// A page of results and the facets.
#[model]
pub struct SearchResults {
    pub items: Vec<ProductCard>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    /// The sort in effect.
    pub sort: String,
    pub facets: Facets,
}

enum Bind {
    Text(String),
    Texts(Vec<String>),
    Int(i64),
}

/// Which filter to leave out, so its own facet counts every value.
#[derive(PartialEq)]
enum Leave<'a> {
    Nothing,
    Tags,
    Price,
    Option(&'a str),
}

/// The `WHERE` of a search on `products p`, and its values from `$2` on
/// (`$1` is the shop).
fn filter(s: &SearchQuery, leave: &Leave<'_>) -> (String, Vec<Bind>) {
    let mut sql = String::from("p.tenant_id = $1 AND p.status = 'active'");
    let mut binds = Vec::new();
    let next = |b: Bind, binds: &mut Vec<Bind>| {
        binds.push(b);
        binds.len() + 1
    };
    if let Some(tq) = tsquery(&s.q) {
        let n = next(Bind::Text(tq), &mut binds);
        sql.push_str(&format!(
            " AND p.search_vector @@ to_tsquery('simple', ${n})"
        ));
    }
    if !s.tags.is_empty() && *leave != Leave::Tags {
        let n = next(Bind::Texts(s.tags.clone()), &mut binds);
        sql.push_str(&format!(" AND p.tags && ${n}"));
    }
    if *leave != Leave::Price {
        if let Some(min) = s.min_price {
            let n = next(Bind::Int(min), &mut binds);
            sql.push_str(&format!(" AND p.price_minor >= ${n}"));
        }
        if let Some(max) = s.max_price {
            let n = next(Bind::Int(max), &mut binds);
            sql.push_str(&format!(" AND p.price_minor <= ${n}"));
        }
    }
    if let Some(c) = &s.collection {
        let n = next(Bind::Text(c.clone()), &mut binds);
        sql.push_str(&format!(
            " AND p.id IN (SELECT cp.product_id FROM collection_products cp \
             JOIN collections c ON c.id = cp.collection_id \
             WHERE c.tenant_id = $1 AND c.handle = ${n})"
        ));
    }
    for (name, values) in &s.options {
        if *leave == Leave::Option(name) {
            continue;
        }
        let a = next(Bind::Text(name.to_lowercase()), &mut binds);
        let b = next(Bind::Texts(values.clone()), &mut binds);
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM product_options o WHERE o.tenant_id = $1 \
             AND o.product_id = p.id AND lower(o.name) = ${a} AND o.option_values && ${b})"
        ));
    }
    (sql, binds)
}

fn bound<'q, O>(
    mut q: QueryAs<'q, Postgres, O, PgArguments>,
    scope: &'q str,
    binds: Vec<Bind>,
) -> QueryAs<'q, Postgres, O, PgArguments> {
    q = q.bind(scope);
    for b in binds {
        q = match b {
            Bind::Text(t) => q.bind(t),
            Bind::Texts(t) => q.bind(t),
            Bind::Int(i) => q.bind(i),
        };
    }
    q
}

/// Runs a search among the shop's active products. Run it in the shop's
/// transaction.
///
/// # Errors
///
/// Database errors.
pub async fn search(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    s: &SearchQuery,
) -> Result<SearchResults> {
    let shop = scope.tenant_id();
    let sort = s.sort();
    let (cond, binds) = filter(s, &Leave::Nothing);
    let (total,): (i64,) = bound(
        sqlx::query_as(&format!("SELECT count(*) FROM products p WHERE {cond}")),
        shop,
        binds,
    )
    .fetch_one(&mut *conn)
    .await?;

    let (cond, binds) = filter(s, &Leave::Nothing);
    let order = match (sort, tsquery(&s.q)) {
        (Sort::Relevance, Some(_)) => {
            // The text is the first value after the shop.
            "ts_rank(p.search_vector, to_tsquery('simple', $2)) DESC, p.id DESC"
        }
        (Sort::PriceLow, _) => "p.price_minor, p.id DESC",
        (Sort::PriceHigh, _) => "p.price_minor DESC, p.id DESC",
        (Sort::Title, _) => "lower(p.title), p.id",
        _ => "p.id DESC",
    };
    let mut items: Vec<ProductCard> = bound(
        sqlx::query_as(&format!(
            "SELECT p.id, p.handle, p.title, p.price_minor, p.currency, p.tags \
             FROM products p WHERE {cond} ORDER BY {order} LIMIT {} OFFSET {}",
            s.limit, s.offset
        )),
        shop,
        binds,
    )
    .fetch_all(&mut *conn)
    .await?;
    first_images(conn, scope, storage, &mut items).await?;

    let (cond, binds) = filter(s, &Leave::Tags);
    let tags: Vec<(String, i64)> = bound(
        sqlx::query_as(&format!(
            "SELECT t, count(*) FROM products p, unnest(p.tags) t WHERE {cond} \
             GROUP BY t ORDER BY count(*) DESC, t LIMIT {FACET_VALUES}"
        )),
        shop,
        binds,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut tags: Vec<FacetValue> = tags
        .into_iter()
        .map(|(value, count)| FacetValue {
            selected: s.tags.contains(&value),
            value,
            count,
        })
        .collect();
    for t in &s.tags {
        if !tags.iter().any(|f| &f.value == t) {
            tags.push(FacetValue {
                value: t.clone(),
                count: 0,
                selected: true,
            });
        }
    }

    // Options: counted once with every filter, and once more for each
    // option a buyer filters by, without that option's own filter.
    let mut options: Vec<OptionFacet> = Vec::new();
    let mut leaves = vec![Leave::Nothing];
    leaves.extend(s.options.iter().map(|(n, _)| Leave::Option(n)));
    for leave in &leaves {
        let (cond, binds) = filter(s, leave);
        let rows: Vec<(String, String, i64)> = bound(
            sqlx::query_as(&format!(
                "SELECT min(o.name), v, count(DISTINCT p.id) FROM products p \
                 JOIN product_options o ON o.tenant_id = p.tenant_id AND o.product_id = p.id, \
                 unnest(o.option_values) v WHERE {cond} \
                 GROUP BY lower(o.name), v ORDER BY lower(o.name), count(DISTINCT p.id) DESC, v"
            )),
            shop,
            binds,
        )
        .fetch_all(&mut *conn)
        .await?;
        for (name, value, count) in rows {
            let chosen = s
                .options
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(&name));
            // Each option's counts come from the pass that leaves out its
            // own filter.
            let own_pass = match (chosen, leave) {
                (Some((n, _)), Leave::Option(l)) => n == l,
                (None, Leave::Nothing) => true,
                _ => false,
            };
            if !own_pass {
                continue;
            }
            let facet = match options
                .iter_mut()
                .find(|o| o.name.eq_ignore_ascii_case(&name))
            {
                Some(f) => f,
                None => {
                    options.push(OptionFacet {
                        name: name.clone(),
                        values: Vec::new(),
                    });
                    options.last_mut().expect("just pushed")
                }
            };
            if facet.values.len() < FACET_VALUES as usize {
                facet.values.push(FacetValue {
                    selected: chosen.is_some_and(|(_, vs)| vs.contains(&value)),
                    value,
                    count,
                });
            }
        }
    }

    let (cond, binds) = filter(s, &Leave::Price);
    let price: Option<(Option<i64>, Option<i64>, Option<String>)> = bound(
        sqlx::query_as(&format!(
            "SELECT min(p.price_minor), max(p.price_minor), \
             mode() WITHIN GROUP (ORDER BY p.currency) FROM products p WHERE {cond}"
        )),
        shop,
        binds,
    )
    .fetch_optional(&mut *conn)
    .await?;
    let price = match price {
        Some((Some(min_minor), Some(max_minor), Some(currency))) => Some(PriceRange {
            min_minor,
            max_minor,
            currency,
        }),
        _ => None,
    };

    Ok(SearchResults {
        items,
        total,
        limit: s.limit,
        offset: s.offset,
        sort: sort.as_str().to_owned(),
        facets: Facets {
            tags,
            options,
            price,
        },
    })
}

/// Search the shop's active products, with facets (no sign-in). See
/// [`SearchQuery`] for the parameters: `q`, `tag`, `option.<Name>`,
/// `min_price`, `max_price` (minor units), `collection`, `sort`, `offset`,
/// `limit`.
#[get("/api/storefront/search", tag = "storefront")]
async fn shop_search(
    db: Db,
    shop: Shop,
    storage: Storage,
    uri: Uri,
) -> Result<Json<SearchResults>> {
    let query = SearchQuery::parse(uri.query().unwrap_or_default());
    let mut tx = shop.begin(&db).await?;
    let found = search(&mut tx, &shop, &storage, &query).await?;
    tx.commit().await?;
    Ok(Json(found))
}

pub(crate) fn mount(app: App) -> App {
    app.mount((shop_search,))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_queries() {
        let s = SearchQuery::parse(
            "q=trail+shoe&tag=a&tag=b&tag=a&option.Size=42&option.size=43&option.Color=red\
             &min_price=1000&max_price=x&sort=-price&offset=-5&limit=500&other=1",
        );
        assert_eq!(s.q, "trail shoe");
        assert_eq!(s.tags, ["a", "b"]);
        assert_eq!(
            s.options,
            [
                ("Size".to_owned(), vec!["42".to_owned(), "43".to_owned()]),
                ("Color".to_owned(), vec!["red".to_owned()])
            ]
        );
        assert_eq!((s.min_price, s.max_price), (Some(1000), None));
        assert_eq!((s.sort(), s.offset, s.limit), (Sort::PriceHigh, 0, 100));
        assert_eq!(SearchQuery::parse("q=x").sort(), Sort::Relevance);
        assert_eq!(SearchQuery::parse("q=%20!").sort(), Sort::Newest);
    }

    #[test]
    fn text_becomes_prefixes() {
        assert_eq!(tsquery("Trail shoe!"), Some("trail:* & shoe:*".into()));
        assert_eq!(
            tsquery("'); DROP TABLE x; --"),
            Some("drop:* & table:* & x:*".into())
        );
        assert_eq!(tsquery("  "), None);
    }
}
