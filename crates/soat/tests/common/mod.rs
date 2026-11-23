//! Test harness: every test gets its own empty Postgres schema.
//!
//! Point `SOAT_TEST_DATABASE_URL` at a Postgres superuser connection
//! (default `postgres://postgres@localhost/postgres`). The harness creates a
//! `soat_test` login role without superuser rights, so row-level security
//! applies to the app exactly as in production, and a `soat_test` database
//! that role owns. Each test then creates its own schema in it and the app
//! connects with that schema as its search path.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use zapline::db::sqlx::postgres::{PgConnectOptions, PgConnection};
use zapline::db::sqlx::{self, ConnectOptions, Connection, Executor};
use zapline::db::Database;
use zapline::test::TestClient;

const ROLE: &str = "soat_test";

fn admin_url() -> String {
    std::env::var("SOAT_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost/postgres".into())
}

/// Runs a statement, ignoring "already exists" from a parallel test.
async fn idempotent(conn: &mut PgConnection, sql: &str) {
    if let Err(e) = conn.execute(sql).await {
        let code = e.as_database_error().and_then(|d| d.code());
        // 42710 duplicate role, 42P04 duplicate database, 23505 a parallel
        // CREATE of the same name.
        if !matches!(code.as_deref(), Some("42710" | "42P04" | "23505")) {
            panic!("test database setup failed on `{sql}`: {e}");
        }
    }
}

/// A database URL for a fresh, empty schema.
pub async fn fresh_database_url() -> String {
    let admin: PgConnectOptions = admin_url()
        .parse()
        .expect("SOAT_TEST_DATABASE_URL is a URL");
    let mut conn = admin.connect().await.unwrap_or_else(|e| {
        panic!(
            "cannot reach Postgres at {}: {e}. Start Postgres or set SOAT_TEST_DATABASE_URL.",
            admin_url()
        )
    });
    idempotent(
        &mut conn,
        &format!("CREATE ROLE {ROLE} LOGIN PASSWORD '{ROLE}' NOSUPERUSER NOBYPASSRLS"),
    )
    .await;
    idempotent(&mut conn, &format!("CREATE DATABASE {ROLE} OWNER {ROLE}")).await;
    conn.close().await.ok();

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let schema = format!(
        "t_{}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let host = admin.get_host();
    let port = admin.get_port();
    let base = format!("postgres://{ROLE}:{ROLE}@{host}:{port}/{ROLE}");
    let mut conn = PgConnection::connect(&base)
        .await
        .expect("connect as soat_test");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&mut conn)
        .await
        .expect("create test schema");
    conn.close().await.ok();
    format!("{base}?options=-c%20search_path%3D{schema}")
}

/// The SOAT app on a fresh schema.
pub async fn client() -> TestClient {
    soat::app(Database::postgres(fresh_database_url().await)).test()
}
