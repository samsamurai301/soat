//! K4: a database upgraded by a newer SOAT stops an older one at startup,
//! before it can misread a table; a restart on the same version is fine.

mod common;

use zapline::db::sqlx::{self, Connection, PgConnection};
use zapline::db::Database;

fn app(url: &str) -> zapline::test::TestClient {
    soat::app(
        Database::postgres(url.to_owned()),
        soat::Settings::for_tests(common::SETUP_TOKEN),
    )
    .test()
}

#[tokio::test]
async fn an_older_build_refuses_a_newer_database() {
    let (first, url) = common::client_and_url().await;
    first.get("/health/live").send().await.assert_status(200);

    // A restart on the same version starts.
    app(&url)
        .get("/health/live")
        .send()
        .await
        .assert_status(200);

    // A newer SOAT migrated the database further.
    let mut conn = PgConnection::connect(&url).await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (29991231000000, 'from a newer soat', TRUE, '\\x00', 0)",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    let older = app(&url);
    let failed = tokio::spawn(async move { older.get("/health/live").send().await.status() })
        .await
        .expect_err("startup must fail");
    let panic = failed.into_panic();
    let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        message.contains("newer than this build")
            && message.contains("29991231000000 from a newer soat"),
        "{message}"
    );
}
