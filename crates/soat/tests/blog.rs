//! D7: the blog, written by staff and read on the storefront.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    shop: &str,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match token {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    res.assert_status(status);
    if status >= 300 || status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn html(client: &TestClient, path: &str, status: u16) -> String {
    let res = client
        .get(path)
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(status);
    res.text()
}

#[tokio::test]
async fn staff_write_posts_and_buyers_read_them() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let staff = |method: &'static str, path: String, body: Value, status: u16| {
        let (client, admin) = (&client, &admin);
        async move { call(client, "acme", Some(admin), method, &path, body, status).await }
    };
    let hello = staff(
        "POST",
        "/api/blog/posts".into(),
        json!({
            "title": "Hello world", "excerpt": "We opened.", "body": "We *opened* a shop.",
            "author": "Sam", "tags": ["News", " news ", "Shop"], "published": true,
            "published_at": "2026-11-01T09:00:00Z",
        }),
        201,
    )
    .await;
    assert_eq!(hello["handle"], "hello-world");
    assert_eq!(hello["tags"], json!(["News", "Shop"]));
    assert_eq!(hello["published_at"], "2026-11-01T09:00:00Z");
    staff(
        "POST",
        "/api/blog/posts".into(),
        json!({ "title": "Recipes", "body": "Soup.", "tags": ["Food"], "published": true }),
        201,
    )
    .await;
    staff(
        "POST",
        "/api/blog/posts".into(),
        json!({ "title": "Secret plans", "body": "Shh." }),
        201,
    )
    .await;
    staff(
        "POST",
        "/api/blog/posts".into(),
        json!({ "title": "Next year", "body": "Soon.", "published": true, "published_at": "2999-01-01T00:00:00Z" }),
        201,
    )
    .await;
    staff(
        "POST",
        "/api/blog/posts".into(),
        json!({ "title": "Hello world", "body": "Again" }),
        409,
    )
    .await;
    for bad in [
        json!({ "title": "", "body": "x" }),
        json!({ "title": "X", "handle": "Not A Handle" }),
        json!({ "title": "X", "published_at": "tomorrow" }),
        json!({ "title": "X", "image_url": "not a url" }),
    ] {
        staff("POST", "/api/blog/posts".into(), bad, 422).await;
    }
    let all = staff("GET", "/api/blog/posts".into(), Value::Null, 200).await;
    assert_eq!(all["total"], 4);
    let news = staff("GET", "/api/blog/posts?tag=News".into(), Value::Null, 200).await;
    assert_eq!(news["total"], 1);

    // Buyers see what is published, newest first; drafts and scheduled
    // posts stay hidden.
    let list = call(
        &client,
        "acme",
        None,
        "GET",
        "/api/storefront/blog",
        Value::Null,
        200,
    )
    .await;
    let titles: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Recipes", "Hello world"]);
    let tagged = call(
        &client,
        "acme",
        None,
        "GET",
        "/api/storefront/blog?tag=news",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(tagged["total"], 1);
    call(
        &client,
        "acme",
        None,
        "GET",
        "/api/storefront/blog/secret-plans",
        Value::Null,
        404,
    )
    .await;
    call(
        &client,
        "acme",
        None,
        "GET",
        "/api/storefront/blog/next-year",
        Value::Null,
        404,
    )
    .await;

    let page = html(&client, "/blog", 200).await;
    assert!(
        page.contains("Hello world") && page.contains("We opened."),
        "{page}"
    );
    assert!(!page.contains("Secret plans") && !page.contains("Next year"));
    let page = html(&client, "/blog?tag=Food", 200).await;
    assert!(page.contains("Recipes") && !page.contains("Hello world"));
    let post = html(&client, "/blog/hello-world", 200).await;
    assert!(post.contains("<em>opened</em>") && post.contains("Sam"));
    assert!(post.contains("2026-11-01"));
    html(&client, "/blog/secret-plans", 404).await;
    let res = client
        .get("/blog/feed.xml")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(200);
    let feed = res.text();
    assert!(feed.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"));
    assert!(feed.contains("<title>Hello world</title>") && !feed.contains("Secret"));
    let map = html(&client, "/sitemap.xml", 200).await;
    assert!(map.contains("/blog/hello-world</loc>") && !map.contains("secret-plans"));

    // Unpublishing hides it again; deleting removes it.
    let id = hello["id"].as_str().unwrap();
    let back = staff(
        "PUT",
        format!("/api/blog/posts/{id}"),
        json!({ "title": "Hello world", "body": "x", "published": false }),
        200,
    )
    .await;
    assert_eq!(back["published"], false);
    html(&client, "/blog/hello-world", 404).await;
    staff("DELETE", format!("/api/blog/posts/{id}"), Value::Null, 204).await;
    staff("GET", format!("/api/blog/posts/{id}"), Value::Null, 404).await;

    // Another shop sees none of it.
    let other = |method: &'static str, path: String, body: Value, status: u16| {
        let (client, admin) = (&client, &admin);
        async move { call(client, "other", Some(admin), method, &path, body, status).await }
    };
    let list = other("GET", "/api/blog/posts".into(), Value::Null, 200).await;
    assert_eq!(list["total"], 0);
    let any = staff("GET", "/api/blog/posts".into(), Value::Null, 200).await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    other("GET", format!("/api/blog/posts/{any}"), Value::Null, 404).await;
    other(
        "PUT",
        format!("/api/blog/posts/{any}"),
        json!({ "title": "Mine", "body": "x" }),
        404,
    )
    .await;
    other("DELETE", format!("/api/blog/posts/{any}"), Value::Null, 404).await;
    call(
        &client,
        "other",
        None,
        "GET",
        "/api/storefront/blog/recipes",
        Value::Null,
        404,
    )
    .await;
    call(
        &client,
        "acme",
        None,
        "GET",
        "/api/blog/posts",
        Value::Null,
        401,
    )
    .await;
}

#[tokio::test]
async fn staff_write_posts_in_the_admin() {
    let (client, _) = common::with_tenants(&["acme"]).await;
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let post = |path: String, form: Value| {
        client
            .post(&path)
            .header("cookie", &cookie)
            .form(&form)
            .send()
    };
    let get = |path: String| client.get(&path).header("cookie", &cookie).send();

    assert!(get("/admin/acme/blog".into())
        .await
        .text()
        .contains("No posts yet"));
    let res = post(
        "/admin/acme/blog".into(),
        json!({ "title": "Spring", "body": "Flowers", "handle": "Bad Handle" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("Flowers"));
    let res = post(
        "/admin/acme/blog".into(),
        json!({ "title": "Spring", "body": "Flowers", "tags": "Garden, news", "published": "on", "date": "2026-03-20T08:30" }),
    )
    .await;
    res.assert_status(303);
    let page = res.header("location").unwrap().to_owned();
    let form = get(page.clone()).await.text();
    assert!(form.contains("Garden, news") && form.contains("value=\"2026-03-20T08:30\""));
    let list = get("/admin/acme/blog".into()).await.text();
    assert!(list.contains("Spring") && list.contains("Published") && list.contains("2026-03-20"));
    assert!(html(&client, "/blog/spring", 200).await.contains("Flowers"));
    post(format!("{page}/delete"), json!({}))
        .await
        .assert_status(303);
    assert!(get("/admin/acme/blog".into())
        .await
        .text()
        .contains("No posts yet"));
}
