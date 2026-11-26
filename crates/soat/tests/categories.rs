//! B4: categories (a tree) and tags.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    token: &str,
    shop: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "PUT" => client.put(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    };
    let req = req.header("x-soat-tenant", shop).bearer(token);
    let req = if method == "GET" || method == "DELETE" {
        req
    } else {
        req.json(&body)
    };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn category(client: &TestClient, token: &str, title: &str, parent: Option<&str>) -> String {
    let c = call(
        client,
        token,
        "acme",
        "POST",
        "/api/categories",
        json!({ "title": title, "parent_id": parent }),
        201,
    )
    .await;
    c["id"].as_str().unwrap().to_owned()
}

async fn product(client: &TestClient, token: &str, sku: &str, tags: Value) -> String {
    let p = call(
        client,
        token,
        "acme",
        "POST",
        "/api/products",
        json!({ "sku": sku, "title": sku, "price_minor": 1, "currency": "EUR", "tags": tags }),
        201,
    )
    .await;
    p["id"].as_str().unwrap().to_owned()
}

fn skus(page: &Value) -> Vec<&str> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["sku"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_category_lists_its_whole_subtree() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let clothing = category(&client, &admin, "Clothing", None).await;
    let shirts = category(&client, &admin, "Shirts", Some(&clothing)).await;
    let polos = category(&client, &admin, "Polos", Some(&shirts)).await;
    let shoes = category(&client, &admin, "Shoes", None).await;

    let polo = product(&client, &admin, "POLO", json!([])).await;
    let tee = product(&client, &admin, "TEE", json!([])).await;
    let boot = product(&client, &admin, "BOOT", json!([])).await;
    for (p, cats) in [
        (&polo, json!([polos])),
        (&tee, json!([shirts, shirts])),
        (&boot, json!([shoes])),
    ] {
        call(
            &client,
            &admin,
            "acme",
            "PUT",
            &format!("/api/products/{p}/categories"),
            json!({ "category_ids": cats }),
            200,
        )
        .await;
    }
    let listed = |id: String| {
        let client = &client;
        let admin = &admin;
        async move {
            call(
                client,
                admin,
                "acme",
                "GET",
                &format!("/api/categories/{id}/products?sort=sku"),
                json!(null),
                200,
            )
            .await
        }
    };
    assert_eq!(skus(&listed(clothing.clone()).await), ["POLO", "TEE"]);
    assert_eq!(skus(&listed(polos.clone()).await), ["POLO"]);

    // Move Shirts under Shoes: the subtree comes along.
    let moved = call(
        &client,
        &admin,
        "acme",
        "PATCH",
        &format!("/api/categories/{shirts}"),
        json!({ "parent_id": shoes }),
        200,
    )
    .await;
    assert_eq!(moved["ancestors"], json!([shoes, shirts]));
    let polos_now = call(
        &client,
        &admin,
        "acme",
        "GET",
        &format!("/api/categories/{polos}"),
        json!(null),
        200,
    )
    .await;
    assert_eq!(polos_now["ancestors"], json!([shoes, shirts, polos]));
    assert_eq!(skus(&listed(shoes.clone()).await), ["BOOT", "POLO", "TEE"]);
    assert!(skus(&listed(clothing.clone()).await).is_empty());

    // No cycles; an unknown parent or category is a 422.
    call(
        &client,
        &admin,
        "acme",
        "PATCH",
        &format!("/api/categories/{shoes}"),
        json!({ "parent_id": polos }),
        422,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "PATCH",
        &format!("/api/categories/{shoes}"),
        json!({ "parent_id": shoes }),
        422,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "POST",
        "/api/categories",
        json!({ "title": "X", "parent_id": "cat_nope" }),
        422,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "PUT",
        &format!("/api/products/{tee}/categories"),
        json!({ "category_ids": ["cat_nope"] }),
        422,
    )
    .await;

    // Back to the root, then delete a subtree.
    call(
        &client,
        &admin,
        "acme",
        "PATCH",
        &format!("/api/categories/{shirts}"),
        json!({ "make_root": true }),
        200,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "DELETE",
        &format!("/api/categories/{shirts}"),
        json!(null),
        204,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "GET",
        &format!("/api/categories/{polos}"),
        json!(null),
        404,
    )
    .await;
    let left = call(
        &client,
        &admin,
        "acme",
        "GET",
        &format!("/api/products/{polo}/categories"),
        json!(null),
        200,
    )
    .await;
    assert_eq!(left["category_ids"], json!([]));
    let all = call(
        &client,
        &admin,
        "acme",
        "GET",
        "/api/categories",
        json!(null),
        200,
    )
    .await;
    assert_eq!(all["total"], 2);
}

#[tokio::test]
async fn trees_have_a_depth_limit() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let mut parent = None;
    for i in 0..8 {
        parent = Some(category(&client, &admin, &format!("L{i}"), parent.as_deref()).await);
    }
    call(
        &client,
        &admin,
        "acme",
        "POST",
        "/api/categories",
        json!({ "title": "L8", "parent_id": parent }),
        422,
    )
    .await;
    // Moving a two-level tree under depth 7 would make depth 9.
    let top = category(&client, &admin, "Top", None).await;
    category(&client, &admin, "Child", Some(&top)).await;
    let all = call(
        &client,
        &admin,
        "acme",
        "GET",
        "/api/categories?q=L6",
        json!(null),
        200,
    )
    .await;
    let l6 = all["items"][0]["id"].as_str().unwrap();
    call(
        &client,
        &admin,
        "acme",
        "PATCH",
        &format!("/api/categories/{top}"),
        json!({ "parent_id": l6 }),
        422,
    )
    .await;
}

#[tokio::test]
async fn products_filter_by_tag() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let a = product(
        &client,
        &admin,
        "A",
        json!([" Summer", "summer", "organic"]),
    )
    .await;
    product(&client, &admin, "B", json!(["winter"])).await;
    let got = call(
        &client,
        &admin,
        "acme",
        "GET",
        &format!("/api/products/{a}"),
        json!(null),
        200,
    )
    .await;
    assert_eq!(got["tags"], json!(["Summer", "organic"]));
    let page = call(
        &client,
        &admin,
        "acme",
        "GET",
        "/api/products?tag=organic",
        json!(null),
        200,
    )
    .await;
    assert_eq!(skus(&page), ["A"]);
    call(
        &client,
        &admin,
        "acme",
        "PATCH",
        &format!("/api/products/{a}"),
        json!({ "tags": ["winter"] }),
        200,
    )
    .await;
    let page = call(
        &client,
        &admin,
        "acme",
        "GET",
        "/api/products?tag=winter&sort=sku",
        json!(null),
        200,
    )
    .await;
    assert_eq!(skus(&page), ["A", "B"]);
}

#[tokio::test]
async fn categories_stay_in_their_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let theirs = call(
        &client,
        &admin,
        "globex",
        "POST",
        "/api/categories",
        json!({ "title": "Theirs" }),
        201,
    )
    .await;
    let theirs = theirs["id"].as_str().unwrap();
    let ours = product(&client, &admin, "A", json!([])).await;
    call(
        &client,
        &admin,
        "acme",
        "GET",
        &format!("/api/categories/{theirs}"),
        json!(null),
        404,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "GET",
        &format!("/api/categories/{theirs}/products"),
        json!(null),
        404,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "DELETE",
        &format!("/api/categories/{theirs}"),
        json!(null),
        404,
    )
    .await;
    // Their category can be neither a parent nor a product's category here.
    call(
        &client,
        &admin,
        "acme",
        "POST",
        "/api/categories",
        json!({ "title": "X", "parent_id": theirs }),
        422,
    )
    .await;
    call(
        &client,
        &admin,
        "acme",
        "PUT",
        &format!("/api/products/{ours}/categories"),
        json!({ "category_ids": [theirs] }),
        422,
    )
    .await;
}
