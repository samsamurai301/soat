//! D9: search with facets.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn staff(client: &TestClient, t: &str, method: &str, path: &str, body: Value) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", "acme")
    .bearer(t);
    let res = if body.is_null() { req } else { req.json(&body) }
        .send()
        .await;
    assert!(res.status().is_success(), "{path}: {}", res.text());
    if res.status().as_u16() == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn search(client: &TestClient, shop: &str, query: &str) -> Value {
    let res = client
        .get(&format!("/api/storefront/search?{query}"))
        .header("x-soat-tenant", shop)
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

fn titles(v: &Value) -> Vec<&str> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect()
}

fn facet<'a>(v: &'a Value, path: &[&str]) -> Vec<(&'a str, i64, bool)> {
    let mut f = &v["facets"];
    for p in path {
        f = match p.parse::<usize>() {
            Ok(i) => &f[i],
            Err(_) => &f[*p],
        };
    }
    f.as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["value"].as_str().unwrap(),
                x["count"].as_i64().unwrap(),
                x["selected"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn buyers_search_and_filter() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let mut ids = Vec::new();
    for (sku, title, price, tags, sizes, status, description) in [
        (
            "TRAIL",
            "Trail Shoe",
            12999,
            vec!["running", "outdoor"],
            vec!["42", "43"],
            "active",
            "Grippy on wet rock.",
        ),
        (
            "ROAD",
            "Road Shoe",
            9999,
            vec!["running"],
            vec!["43", "44"],
            "active",
            "Light.",
        ),
        (
            "HAT",
            "Wool Hat",
            2500,
            vec!["winter"],
            vec![],
            "active",
            "Warm.",
        ),
        (
            "BOOT",
            "Draft Boot",
            5000,
            vec!["running"],
            vec!["43"],
            "draft",
            "",
        ),
    ] {
        let p = staff(
            &client,
            &t,
            "POST",
            "/api/products",
            json!({ "sku": sku, "title": title, "price_minor": price, "currency": "EUR",
                    "tags": tags, "status": status, "description": description }),
        )
        .await;
        let id = p["id"].as_str().unwrap().to_owned();
        if !sizes.is_empty() {
            staff(
                &client,
                &t,
                "PUT",
                &format!("/api/products/{id}/options"),
                json!({ "options": [{ "name": "Size", "values": sizes }] }),
            )
            .await;
        }
        ids.push(id);
    }

    // Words match by prefix, in titles, SKUs, tags and descriptions.
    let r = search(&client, "acme", "q=sho").await;
    assert_eq!(r["total"], 2, "{r}");
    assert_eq!(r["sort"], "relevance");
    assert_eq!(
        titles(&search(&client, "acme", "q=grippy").await),
        ["Trail Shoe"]
    );
    assert_eq!(
        titles(&search(&client, "acme", "q=road").await),
        ["Road Shoe"]
    );
    assert_eq!(
        titles(&search(&client, "acme", "q=WINTER").await),
        ["Wool Hat"]
    );
    assert_eq!(
        search(&client, "acme", "q=boot").await["total"],
        0,
        "drafts stay hidden"
    );
    assert_eq!(
        search(&client, "acme", "q=%27%3B+--").await["total"],
        3,
        "no words: all"
    );

    // Tags: choosing one keeps the others' counts.
    let r = search(&client, "acme", "tag=running").await;
    assert_eq!(r["total"], 2);
    assert_eq!(
        facet(&r, &["tags"]),
        [
            ("running", 2, true),
            ("outdoor", 1, false),
            ("winter", 1, false)
        ]
    );
    assert_eq!(
        facet(&r, &["options", "0", "values"])
            .iter()
            .map(|(v, n, _)| (*v, *n))
            .collect::<Vec<_>>(),
        [("43", 2), ("42", 1), ("44", 1)]
    );

    // Options, case-insensitive by name; their own counts stay whole.
    let r = search(&client, "acme", "option.size=44").await;
    assert_eq!(titles(&r), ["Road Shoe"]);
    assert_eq!(r["facets"]["options"][0]["name"], "Size");
    assert_eq!(
        facet(&r, &["options", "0", "values"]),
        [("43", 2, false), ("42", 1, false), ("44", 1, true)]
    );
    assert_eq!(facet(&r, &["tags"]), [("running", 1, false)]);
    let r = search(&client, "acme", "option.Size=42&option.Size=44").await;
    assert_eq!(r["total"], 2);

    // Price, in minor units; the range ignores its own filter.
    let r = search(&client, "acme", "min_price=5000&sort=price").await;
    assert_eq!(titles(&r), ["Road Shoe", "Trail Shoe"]);
    assert_eq!(
        r["facets"]["price"],
        json!({ "min_minor": 2500, "max_minor": 12999, "currency": "EUR" })
    );
    assert_eq!(
        titles(&search(&client, "acme", "sort=-price&limit=2").await),
        ["Trail Shoe", "Road Shoe"]
    );
    assert_eq!(
        titles(&search(&client, "acme", "sort=title&offset=1").await),
        ["Trail Shoe", "Wool Hat"]
    );

    // A collection.
    let col = staff(
        &client,
        &t,
        "POST",
        "/api/collections",
        json!({ "title": "Road" }),
    )
    .await;
    staff(
        &client,
        &t,
        "PUT",
        &format!(
            "/api/collections/{}/products/{}",
            col["id"].as_str().unwrap(),
            ids[1]
        ),
        Value::Null,
    )
    .await;
    assert_eq!(
        titles(&search(&client, "acme", "collection=road").await),
        ["Road Shoe"]
    );

    // Another shop finds nothing.
    let theirs = search(&client, "globex", "q=shoe").await;
    assert_eq!(theirs["total"], 0);
    assert_eq!(theirs["facets"]["price"], Value::Null);

    // The page: filters as a form, prices in euros.
    let page = client
        .get("/search?q=shoe&option.Size=44&price_from=&sort=price")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    page.assert_status(200);
    let html = page.text();
    assert!(
        html.contains("Road Shoe") && !html.contains("Trail Shoe"),
        "{html}"
    );
    assert!(html.contains("name=\"option.Size\" value=\"44\" checked"));
    assert!(html.contains("<option value=\"price\" selected>"));
    assert!(
        html.contains("placeholder=\"99.99\""),
        "the cheapest shoe: {html}"
    );
    assert!(html.contains("Clear filters"));
    let html = client
        .get("/search?price_from=100")
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .text();
    assert!(html.contains("Trail Shoe") && !html.contains("Road Shoe"));
    assert!(html.contains("value=\"100\""));
}
