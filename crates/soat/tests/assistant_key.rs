//! H4: a shop's own Anthropic key, sealed at rest.

mod common;

use serde_json::{json, Value};
use soat_core::secret::Sealer;
use zapline::db::sqlx::{self, PgPool};
use zapline::db::Database;

/// The secret `Settings::for_tests` seals with.
const TEST_SECRET: &str = "the test server's secret, long enough to use";

#[tokio::test]
async fn a_shop_keeps_its_own_anthropic_key() {
    let (client, url) = common::client_and_url().await;
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme", "globex"]).await;
    let key = |shop: &'static str, method: &'static str, body: Option<Value>| {
        let req = match method {
            "PUT" => client.put("/api/assistant/key"),
            "DELETE" => client.delete("/api/assistant/key"),
            _ => client.get("/api/assistant/key"),
        }
        .header("x-soat-tenant", shop)
        .bearer(&t);
        async move {
            match body {
                Some(b) => req.json(&b).send().await,
                None => req.send().await,
            }
        }
    };
    let res = key("acme", "GET", None).await;
    res.assert_status(200);
    assert_eq!(res.json::<Value>()["set"], false);
    key(
        "acme",
        "PUT",
        Some(json!({ "api_key": "not-a-key-but-long-enough", "model": "m" })),
    )
    .await
    .assert_status(422);
    key(
        "acme",
        "PUT",
        Some(json!({ "api_key": "sk-ant-api03-abcdefghijWXYZ", "model": "a model" })),
    )
    .await
    .assert_status(422);
    let secret = "sk-ant-api03-abcdefghijWXYZ";
    let res = key(
        "acme",
        "PUT",
        Some(json!({ "api_key": secret, "model": "claude-test" })),
    )
    .await;
    res.assert_status(200);
    let v = res.json::<Value>();
    assert_eq!(v["set"], true);
    assert_eq!(v["last4"], "WXYZ");
    assert_eq!(v["model"], "claude-test");
    assert!(!v.to_string().contains(secret));

    // Sealed at rest, for this shop only.
    let pool = PgPool::connect(&url).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', 'acme', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let (sealed,): (Vec<u8>,) = sqlx::query_as("SELECT sealed FROM assistant_keys")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(!sealed.windows(secret.len()).any(|w| w == secret.as_bytes()));
    let sealer = Sealer::from_secret(TEST_SECRET).unwrap();
    struct In(&'static str);
    impl soat_tenancy::Scope for In {
        fn tenant_id(&self) -> &str {
            self.0
        }
        fn actor_id(&self) -> Option<&str> {
            None
        }
    }
    let opened = soat_mcp::keys::anthropic_key(&mut tx, &In("acme"), &sealer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(opened.api_key, secret);
    assert_eq!(opened.model, "claude-test");
    assert!(!format!("{opened:?}").contains(secret));
    // Moved to another shop's row, it does not open.
    assert!(sealer.open(&sealed, "globex").is_none());
    tx.rollback().await.unwrap();

    assert_eq!(
        key("globex", "GET", None).await.json::<Value>()["set"],
        false
    );
    let events = client
        .get("/api/events?kind=assistant_key.set")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .text();
    assert!(
        events.contains("WXYZ") && !events.contains(secret),
        "{events}"
    );

    // Staff do not manage it.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "email": "staff@example.com", "password": "a long password", "role": "staff" }))
        .send()
        .await
        .assert_status(201);
    let staff = client
        .post("/api/auth/login")
        .json(&json!({ "email": "staff@example.com", "password": "a long password" }))
        .send()
        .await
        .json::<Value>()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    client
        .get("/api/assistant/key")
        .header("x-soat-tenant", "acme")
        .bearer(&staff)
        .send()
        .await
        .assert_status(403);

    // In the admin.
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let me = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = client
        .get("/admin/acme/settings/assistant")
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(
        html.contains("A key ending in <code>WXYZ</code>") && !html.contains(secret),
        "{html}"
    );
    client
        .post("/admin/globex/settings/assistant/key")
        .header("cookie", &me)
        .form(&json!({ "api_key": "sk-ant-api03-globexglobex1234", "model": "claude-test" }))
        .send()
        .await
        .assert_status(303);
    assert_eq!(
        key("globex", "GET", None).await.json::<Value>()["last4"],
        "1234"
    );

    key("acme", "DELETE", None).await.assert_status(204);
    key("acme", "DELETE", None).await.assert_status(404);
    assert_eq!(key("acme", "GET", None).await.json::<Value>()["set"], false);
}

#[tokio::test]
async fn without_a_server_secret_no_key_is_stored() {
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.sealer = None;
    let client = soat::app(Database::postgres(url), settings).test();
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme"]).await;
    let res = client
        .put("/api/assistant/key")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "api_key": "sk-ant-api03-abcdefghijWXYZ", "model": "claude-test" }))
        .send()
        .await;
    res.assert_status(409);
    assert!(res.text().contains("SOAT_SECRET_KEY"));
}
