//! List endpoints inside a tenant transaction.
//!
//! The same query-string contract as Zapline's `Listing` (`limit`, `offset`,
//! `sort`, `q`, one parameter per filter, answered as `Paged<T>`), but run on
//! the request's [`TenantTx`](crate::TenantTx) instead of the pool, so
//! row-level security applies. `Listing::fetch` only takes a pool; once
//! Zapline can run it on a connection, this file goes away.

use zapline::db::sqlx::postgres::PgRow;
use zapline::db::sqlx::{FromRow, PgConnection, Postgres, QueryBuilder};
use zapline::db::{ListParams, Paged};
use zapline::prelude::*;

/// A list over one tenant-owned table. Column names come from code; request
/// values are always bound as parameters.
#[derive(Clone, Debug)]
pub struct TenantListing {
    table: &'static str,
    columns: &'static str,
    key: &'static str,
    default_sort: &'static str,
    sortable: Vec<&'static str>,
    search: Vec<&'static str>,
    text_filters: Vec<&'static str>,
    scopes: Vec<(&'static str, String)>,
    default_limit: i64,
    max_limit: i64,
}

impl TenantListing {
    /// List `columns` of `table`, sorted by `id`, 50 per page (at most 200).
    pub fn new(table: &'static str, columns: &'static str) -> Self {
        Self {
            table,
            columns,
            key: "id",
            default_sort: "id",
            sortable: Vec::new(),
            search: Vec::new(),
            text_filters: Vec::new(),
            scopes: Vec::new(),
            default_limit: 50,
            max_limit: 200,
        }
    }

    /// Columns callers may sort by (`?sort=-price_minor,title`).
    pub fn sortable(mut self, columns: impl IntoIterator<Item = &'static str>) -> Self {
        self.sortable.extend(columns);
        self
    }

    /// The sort when the request names none, e.g. `-id` for newest first.
    pub fn default_sort(mut self, sort: &'static str) -> Self {
        self.default_sort = sort;
        self
    }

    /// Columns `?q=` searches, case-insensitively.
    pub fn search(mut self, columns: impl IntoIterator<Item = &'static str>) -> Self {
        self.search.extend(columns);
        self
    }

    /// A text column callers may filter on exactly (`?currency=EUR`).
    pub fn filter_text(mut self, column: &'static str) -> Self {
        self.text_filters.push(column);
        self
    }

    /// Always restricts `column` to `value`, e.g. the parent's id.
    pub fn scope_text(mut self, column: &'static str, value: impl Into<String>) -> Self {
        self.scopes.push((column, value.into()));
        self
    }

    /// Default and maximum page size.
    pub fn page_size(mut self, default: i64, max: i64) -> Self {
        self.default_limit = default;
        self.max_limit = max;
        self
    }

    /// Runs the list on `conn`, normally `&mut *tx` of a tenant transaction.
    ///
    /// # Errors
    ///
    /// `400` for an unknown sort column (`invalid_sort`) or a bad
    /// `limit`/`offset` (`invalid_page`); database errors otherwise.
    pub async fn fetch<T>(&self, conn: &mut PgConnection, params: &ListParams) -> Result<Paged<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        let limit = match params.get("limit") {
            None | Some("") => self.default_limit,
            Some(v) => parse_page(v, "limit")?.clamp(1, self.max_limit),
        };
        let offset = match params.get("offset") {
            None | Some("") => 0,
            Some(v) => parse_page(v, "offset")?,
        };
        let order = self.order_by(params.get("sort").filter(|s| !s.is_empty()))?;

        let mut count =
            QueryBuilder::<Postgres>::new(format!("SELECT COUNT(*) FROM {}", self.table));
        self.push_where(&mut count, params);
        let (total,): (i64,) = count.build_query_as().fetch_one(&mut *conn).await?;

        let mut select =
            QueryBuilder::<Postgres>::new(format!("SELECT {} FROM {}", self.columns, self.table));
        self.push_where(&mut select, params);
        select.push(format!(" ORDER BY {order} LIMIT "));
        select.push_bind(limit);
        select.push(" OFFSET ");
        select.push_bind(offset);
        let items = select.build_query_as::<T>().fetch_all(&mut *conn).await?;

        Ok(Paged {
            items,
            total,
            limit,
            offset,
        })
    }

    fn push_where(&self, q: &mut QueryBuilder<'_, Postgres>, params: &ListParams) {
        // Row-level security already limits rows to the tenant; this list
        // adds only the caller's filters.
        q.push(" WHERE TRUE");
        for (column, value) in &self.scopes {
            q.push(format!(" AND {column} = "));
            q.push_bind(value.clone());
        }
        for column in &self.text_filters {
            if let Some(value) = params.get(column).filter(|v| !v.is_empty()) {
                q.push(format!(" AND {column} = "));
                q.push_bind(value.to_owned());
            }
        }
        let query = params.get("q").map(str::trim).filter(|q| !q.is_empty());
        if let (Some(text), false) = (query, self.search.is_empty()) {
            let pattern = format!("%{}%", escape_like(text));
            q.push(" AND (");
            for (i, column) in self.search.iter().enumerate() {
                if i > 0 {
                    q.push(" OR ");
                }
                q.push(format!("{column} ILIKE "));
                q.push_bind(pattern.clone());
            }
            q.push(")");
        }
    }

    fn order_by(&self, sort: Option<&str>) -> Result<String> {
        let mut parts = Vec::new();
        let mut has_key = false;
        for item in sort.unwrap_or(self.default_sort).split(',').map(str::trim) {
            if item.is_empty() {
                continue;
            }
            let (column, dir) = match item.strip_prefix('-') {
                Some(c) => (c, "DESC"),
                None => (item.strip_prefix('+').unwrap_or(item), "ASC"),
            };
            if column != self.key && !self.sortable.contains(&column) {
                return Err(Error::bad_request(format!(
                    "can't sort by {column:?}; sortable: {}, {}",
                    self.key,
                    self.sortable.join(", ")
                ))
                .with_code("invalid_sort"));
            }
            has_key |= column == self.key;
            parts.push(format!("{column} {dir}"));
        }
        if !has_key {
            parts.push(format!("{} ASC", self.key));
        }
        Ok(parts.join(", "))
    }
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn parse_page(v: &str, name: &str) -> Result<i64> {
    match v.parse::<i64>() {
        Ok(n) if n >= 0 => Ok(n),
        _ => Err(
            Error::bad_request(format!("{name} must be a non-negative integer"))
                .with_code("invalid_page"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_is_allow_listed_and_always_ends_on_the_key() {
        let l = TenantListing::new("products", "id").sortable(["title"]);
        assert_eq!(l.order_by(None).unwrap(), "id ASC");
        assert_eq!(l.order_by(Some("-title")).unwrap(), "title DESC, id ASC");
        assert!(l.order_by(Some("tenant_id")).is_err());
        assert!(l.order_by(Some("title; DROP TABLE x")).is_err());
    }

    #[test]
    fn like_wildcards_in_searches_are_literal() {
        assert_eq!(escape_like("50%_off\\"), "50\\%\\_off\\\\");
    }
}
