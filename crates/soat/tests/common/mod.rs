//! Helpers shared by the end-to-end tests: the SOAT app on a fresh schema.

#![allow(dead_code)]

use zapline::db::Database;
use zapline::test::TestClient;

pub use soat_testing::fresh_database_url;

/// The SOAT app on a fresh schema.
pub async fn client() -> TestClient {
    client_and_url().await.0
}

/// The setup token every test app uses.
pub const SETUP_TOKEN: &str = "test-setup-token";

/// The first platform admin's credentials in every test app.
pub const ADMIN_EMAIL: &str = "admin@example.com";
pub const ADMIN_PASSWORD: &str = "correct horse battery";

/// The SOAT app on a fresh schema, and that schema's URL for direct SQL.
pub async fn client_and_url() -> (TestClient, String) {
    let url = fresh_database_url().await;
    let auth = soat::Auth::with_setup_token(SETUP_TOKEN);
    (soat::app(Database::postgres(url.clone()), auth).test(), url)
}

/// Runs first-time setup and returns the platform admin's session token.
pub async fn setup_admin(client: &TestClient) -> String {
    let session: serde_json::Value = client
        .post("/api/setup")
        .json(&serde_json::json!({
            "setup_token": SETUP_TOKEN,
            "email": ADMIN_EMAIL,
            "password": ADMIN_PASSWORD,
        }))
        .send()
        .await
        .assert_status(201)
        .json();
    session["token"].as_str().unwrap().to_owned()
}

/// A fresh app with a platform admin who created `ids` (and owns them).
/// Returns the client and the admin's token.
pub async fn with_tenants(ids: &[&str]) -> (TestClient, String) {
    let (client, _) = client_and_url().await;
    let token = setup_admin(&client).await;
    create_tenants(&client, &token, ids).await;
    (client, token)
}

/// Creates shops as the platform admin behind `token`.
pub async fn create_tenants(client: &TestClient, token: &str, ids: &[&str]) {
    for id in ids {
        client
            .post("/api/tenants")
            .bearer(token)
            .json(&serde_json::json!({ "id": id, "name": id }))
            .send()
            .await
            .assert_status(201);
    }
}
