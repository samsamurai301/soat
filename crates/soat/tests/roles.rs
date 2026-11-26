//! Roles and permissions: built-in owner and staff, custom roles, and no
//! way to hand out more than you have.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

const PASSWORD: &str = "member password 1";

async fn member(client: &TestClient, granter: &str, email: &str, role: &str) -> (String, String) {
    let m: Value = client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(granter)
        .json(&json!({ "email": email, "password": PASSWORD, "role": role }))
        .send()
        .await
        .assert_status(201)
        .json();
    let session: Value = client
        .post("/api/auth/login")
        .json(&json!({ "email": email, "password": PASSWORD }))
        .send()
        .await
        .assert_status(200)
        .json();
    (
        m["user_id"].as_str().unwrap().to_owned(),
        session["token"].as_str().unwrap().to_owned(),
    )
}

async fn put_role(client: &TestClient, token: &str, name: &str, permissions: &[&str]) -> u16 {
    client
        .put(&format!("/api/roles/{name}"))
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .json(&json!({ "permissions": permissions }))
        .send()
        .await
        .status()
        .as_u16()
}

fn product() -> Value {
    json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" })
}

#[tokio::test]
async fn permissions_and_built_in_roles_are_listed() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let perms: Vec<Value> = client
        .get("/api/permissions")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    let names: Vec<_> = perms.iter().map(|p| p["name"].as_str().unwrap()).collect();
    for expected in [
        "staff:read",
        "staff:manage",
        "products:read",
        "products:write",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }

    let roles: Vec<Value> = client
        .get("/api/roles")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(roles[0]["name"], "owner");
    assert_eq!(roles[1]["name"], "staff");
    assert!(!roles[1]["permissions"]
        .as_array()
        .unwrap()
        .contains(&json!("staff:manage")));
}

#[tokio::test]
async fn a_custom_role_limits_what_a_member_can_do() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    assert_eq!(
        put_role(&client, &admin, "viewer", &["products:read"]).await,
        200
    );
    let (_, viewer) = member(&client, &admin, "viewer@example.com", "viewer").await;

    client
        .get("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&viewer)
        .send()
        .await
        .assert_status(200);
    let refused: Value = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&viewer)
        .json(&product())
        .send()
        .await
        .assert_status(403)
        .json();
    assert_eq!(refused["code"], "missing_permission");
}

#[tokio::test]
async fn roles_are_validated_and_owner_only() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    assert_eq!(put_role(&client, &admin, "owner", &[]).await, 422);
    assert_eq!(put_role(&client, &admin, "BadName", &[]).await, 422);
    assert_eq!(put_role(&client, &admin, "x1", &["nope:never"]).await, 422);

    let (_, clerk) = member(&client, &admin, "clerk@example.com", "staff").await;
    assert_eq!(
        put_role(&client, &clerk, "mine", &["products:read"]).await,
        403
    );
}

#[tokio::test]
async fn nobody_grants_more_than_they_have() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    assert_eq!(
        put_role(&client, &admin, "hr", &["staff:read", "staff:manage"]).await,
        200
    );
    assert_eq!(
        put_role(
            &client,
            &admin,
            "merch",
            &["products:read", "products:write"]
        )
        .await,
        200
    );
    let (_, hr) = member(&client, &admin, "hr@example.com", "hr").await;

    // hr can add people with roles inside its own permissions...
    let (clerk_id, _) = member(&client, &hr, "a@example.com", "hr").await;
    // ...but not roles with permissions it lacks, and never owners.
    for role in ["merch", "staff", "owner"] {
        client
            .post("/api/staff")
            .header("x-soat-tenant", "acme")
            .bearer(&hr)
            .json(&json!({ "email": format!("{role}@example.com"), "password": PASSWORD, "role": role }))
            .send()
            .await
            .assert_status(403);
    }
    // An unknown role is a 422.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "email": "x@example.com", "password": PASSWORD, "role": "ghost" }))
        .send()
        .await
        .assert_status(422);

    // Role changes follow the same rule, and nobody changes their own role.
    client
        .patch(&format!("/api/staff/{clerk_id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&hr)
        .json(&json!({ "role": "merch" }))
        .send()
        .await
        .assert_status(403);
    client
        .patch(&format!("/api/staff/{clerk_id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "role": "merch" }))
        .send()
        .await
        .assert_status(200);

    // A role in use cannot be deleted.
    client
        .delete("/api/roles/merch")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(409);
}

#[tokio::test]
async fn nobody_changes_their_own_role() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let me: Value = client
        .get("/api/auth/me")
        .bearer(&admin)
        .send()
        .await
        .json();
    client
        .patch(&format!("/api/staff/{}", me["id"].as_str().unwrap()))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "role": "staff" }))
        .send()
        .await
        .assert_status(403);
}
