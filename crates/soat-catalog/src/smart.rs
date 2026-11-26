//! Smart collections: rules pick the products, and membership follows as
//! products change. Members live in `collection_products` like hand-picked
//! ones, so lists, search and the storefront read both alike.

use soat_core::rules::{kind, Kind, Rule};
use zapline::db::sqlx::{self, PgConnection, Postgres, QueryBuilder};
use zapline::prelude::*;

/// Adds `(…)`, true for products of alias `p` that meet `rules` (all of
/// them, or any when `all` is false).
fn condition(q: &mut QueryBuilder<'_, Postgres>, rules: &[Rule], all: bool) {
    if rules.is_empty() {
        q.push("false");
        return;
    }
    q.push("(");
    for (i, r) in rules.iter().enumerate() {
        if i > 0 {
            q.push(if all { " AND " } else { " OR " });
        }
        let column = match r.field.as_str() {
            "title" => "p.title",
            "sku" => "p.sku",
            "tax_class" => "p.tax_class",
            "price_minor" => "p.price_minor",
            "weight_grams" => "COALESCE(p.weight_grams, 0)",
            _ => "",
        };
        match (kind(&r.field), r.op.as_str()) {
            (Some(Kind::Tag), op) => {
                q.push(if op == "equals" {
                    "EXISTS"
                } else {
                    "NOT EXISTS"
                });
                q.push(" (SELECT 1 FROM unnest(p.tags) AS t WHERE lower(t) = lower(");
                q.push_bind(r.value.clone());
                q.push("))");
            }
            (Some(Kind::Text), op) => {
                let (head, tail) = match op {
                    "equals" => ("lower(", ") = lower("),
                    "not_equals" => ("lower(", ") <> lower("),
                    "contains" => ("strpos(lower(", "), lower("),
                    // NOT binds looser than `>`: NOT (strpos(…) > 0).
                    "not_contains" => ("NOT strpos(lower(", "), lower("),
                    _ => ("starts_with(lower(", "), lower("),
                };
                q.push(format!("({head}{column}{tail}"));
                q.push_bind(r.value.clone());
                q.push(match op {
                    "contains" | "not_contains" => ")) > 0)",
                    "starts_with" => ")))",
                    _ => "))",
                });
            }
            (Some(Kind::Number), op) => {
                let cmp = match op {
                    "greater_than" => " > ",
                    "less_than" => " < ",
                    _ => " = ",
                };
                q.push(format!("({column}{cmp}"));
                q.push_bind(r.value.parse::<i64>().unwrap_or(0));
                q.push(")");
            }
            (None, _) => {
                q.push("false");
            }
        }
    }
    q.push(")");
}

/// Makes the collection's members exactly the products its rules pick,
/// ordered by title.
pub(crate) async fn refill(
    conn: &mut PgConnection,
    tenant_id: &str,
    collection_id: &str,
    rules: &[Rule],
    all: bool,
) -> Result<u64> {
    sqlx::query("DELETE FROM collection_products WHERE tenant_id = $1 AND collection_id = $2")
        .bind(tenant_id)
        .bind(collection_id)
        .execute(&mut *conn)
        .await?;
    let mut q = QueryBuilder::<Postgres>::new(
        "INSERT INTO collection_products (tenant_id, collection_id, product_id, position) \
         SELECT p.tenant_id, ",
    );
    q.push_bind(collection_id.to_owned());
    q.push(", p.id, ROW_NUMBER() OVER (ORDER BY p.title, p.id) - 1 FROM products p WHERE p.tenant_id = ");
    q.push_bind(tenant_id.to_owned());
    q.push(" AND ");
    condition(&mut q, rules, all);
    Ok(q.build().execute(&mut *conn).await?.rows_affected())
}

/// Puts product `product_id` in or out of each smart collection, after it
/// was created or changed. A newcomer goes last.
pub(crate) async fn place(
    conn: &mut PgConnection,
    tenant_id: &str,
    product_id: &str,
) -> Result<()> {
    let smart: Vec<(String, sqlx::types::Json<Vec<Rule>>, bool)> = sqlx::query_as(
        "SELECT id, rules, match_all FROM collections \
         WHERE tenant_id = $1 AND rules IS NOT NULL",
    )
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await?;
    for (collection, rules, all) in smart {
        let mut q = QueryBuilder::<Postgres>::new(
            "SELECT EXISTS (SELECT 1 FROM products p WHERE p.tenant_id = ",
        );
        q.push_bind(tenant_id.to_owned());
        q.push(" AND p.id = ");
        q.push_bind(product_id.to_owned());
        q.push(" AND ");
        condition(&mut q, &rules.0, all);
        q.push(")");
        let (fits,): (bool,) = q.build_query_as().fetch_one(&mut *conn).await?;
        if fits {
            sqlx::query(
                "INSERT INTO collection_products (tenant_id, collection_id, product_id, position) \
                 SELECT $1, $2, $3, COALESCE(MAX(position) + 1, 0) FROM collection_products \
                 WHERE tenant_id = $1 AND collection_id = $2 \
                 ON CONFLICT (collection_id, product_id) DO NOTHING",
            )
            .bind(tenant_id)
            .bind(&collection)
            .bind(product_id)
            .execute(&mut *conn)
            .await?;
        } else {
            sqlx::query(
                "DELETE FROM collection_products \
                 WHERE tenant_id = $1 AND collection_id = $2 AND product_id = $3",
            )
            .bind(tenant_id)
            .bind(&collection)
            .bind(product_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}
