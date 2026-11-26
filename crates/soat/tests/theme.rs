//! D1–D2: the theme's files and settings.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    t: &str,
    shop: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "PUT" => client.put(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(t);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    res.assert_status(status);
    if status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

#[tokio::test]
async fn shops_edit_their_theme() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let files = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/theme/files",
        Value::Null,
        200,
    )
    .await;
    let names: Vec<&str> = files
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    for n in ["layout", "index", "product", "header", "theme.css"] {
        assert!(names.contains(&n), "{n}");
    }
    let header = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/theme/files/header",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(header["customized"], false);
    assert_eq!(header["source"], header["default_source"]);

    let e = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/theme/files/header",
        json!({ "source": "{{#if x}}" }),
        422,
    )
    .await;
    assert_eq!(e["code"], "invalid_theme_file");
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/theme/files/layout",
        json!({ "source": "<body></body>" }),
        422,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/theme/files/evil",
        json!({ "source": "" }),
        404,
    )
    .await;

    let saved = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/theme/files/header",
        json!({ "source": "<header>{{ shop.name }}</header>" }),
        200,
    )
    .await;
    assert_eq!(saved["customized"], true);
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/theme/files/snippet-badges",
        json!({ "source": "<p>Secure checkout</p>" }),
        200,
    )
    .await;
    let files = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/theme/files",
        Value::Null,
        200,
    )
    .await;
    assert!(files
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["name"] == "snippet-badges"));
    // The other shop keeps the default.
    let theirs = send(
        &client,
        &t,
        "globex",
        "GET",
        "/api/theme/files/header",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(theirs["customized"], false);
    send(
        &client,
        &t,
        "globex",
        "GET",
        "/api/theme/files/snippet-badges",
        Value::Null,
        404,
    )
    .await;

    send(
        &client,
        &t,
        "acme",
        "DELETE",
        "/api/theme/files/header",
        Value::Null,
        204,
    )
    .await;
    let header = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/theme/files/header",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(header["customized"], false);

    // Settings.
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/theme/settings",
        json!({ "accent_color": "red; } body {" }),
        422,
    )
    .await;
    let s = send(&client, &t, "acme", "PUT", "/api/theme/settings",
        json!({ "accent_color": "#0a7", "announcement": "Free shipping over €50", "language": "de-AT" }), 200).await;
    assert_eq!(s["language"], "de-AT");
    let s = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/theme/settings",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(s["accent_color"], "#0a7");
    assert_eq!(
        send(
            &client,
            &t,
            "globex",
            "GET",
            "/api/theme/settings",
            Value::Null,
            200
        )
        .await["accent_color"],
        Value::Null
    );

    // Staff may look but not change it.
    let key: Value = client
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "name": "designer", "permissions": ["theme:read"] }))
        .send()
        .await
        .assert_status(201)
        .json();
    let secret = key["secret"].as_str().unwrap();
    client
        .get("/api/theme/files/header")
        .bearer(secret)
        .send()
        .await
        .assert_status(200);
    client
        .put("/api/theme/files/header")
        .bearer(secret)
        .json(&json!({ "source": "x" }))
        .send()
        .await
        .assert_status(403);

    let events = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/events?kind=theme_file.updated",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["total"], 2);
}
