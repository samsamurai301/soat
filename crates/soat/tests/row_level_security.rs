//! Postgres itself keeps tenants apart, even for a query with no
//! `WHERE tenant_id`. These tests connect as the same non-superuser role as
//! the app and bypass the API on purpose.

mod common;

use serde_json::json;
use zapline::db::sqlx::{self, Connection, PgConnection};

async fn setup() -> PgConnection {
    let (client, url) = common::client_and_url().await;
    for tenant in ["acme", "globex"] {
        client
            .post("/api/tenants")
            .json(&json!({ "id": tenant, "name": tenant }))
            .send()
            .await
            .assert_status(201);
        client
            .post("/api/products")
            .header("x-soat-tenant", tenant)
            .json(&json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" }))
            .send()
            .await
            .assert_status(201);
    }
    PgConnection::connect(&url).await.unwrap()
}

#[tokio::test]
async fn without_a_tenant_no_rows_are_visible() {
    let mut conn = setup().await;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM products")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn a_tenant_transaction_sees_only_its_own_rows() {
    let mut conn = setup().await;
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', 'acme', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let owners: Vec<(String,)> = sqlx::query_as("SELECT tenant_id FROM products")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(owners, [("acme".to_owned(),)]);

    // Writing a row for another tenant is refused.
    let foreign = sqlx::query(
        "INSERT INTO products (id, tenant_id, sku, title, price_minor, currency) \
         VALUES ('prod_00000000000000000000000000', 'globex', 'X', 'X', 1, 'EUR')",
    )
    .execute(&mut *tx)
    .await;
    assert!(foreign.is_err(), "RLS must reject a foreign tenant_id");
}

#[tokio::test]
async fn the_setting_ends_with_the_transaction() {
    let mut conn = setup().await;
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', 'acme', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM products")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// Every table with a `tenant_id` column must have row-level security
/// enabled and forced. A new module that forgets its policy fails here.
#[tokio::test]
async fn every_tenant_owned_table_has_forced_row_level_security() {
    let mut conn = setup().await;
    let unprotected: Vec<(String,)> = sqlx::query_as(
        "SELECT c.relname::text FROM pg_class c \
         JOIN information_schema.columns col \
           ON col.table_name = c.relname AND col.table_schema = current_schema() \
         WHERE c.relnamespace = current_schema()::regnamespace \
           AND col.column_name = 'tenant_id' \
           AND NOT (c.relrowsecurity AND c.relforcerowsecurity)",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert!(
        unprotected.is_empty(),
        "tables without forced RLS: {unprotected:?}"
    );
}
