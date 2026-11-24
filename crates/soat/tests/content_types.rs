//! D12: content types a shop defines for itself, their entries, and the
//! public ones on the storefront API.

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
        "POST" => client.post(path),
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

async fn public(client: &TestClient, shop: &str, path: &str, status: u16) -> Value {
    let res = client.get(path).header("x-soat-tenant", shop).send().await;
    res.assert_status(status);
    res.json()
}

fn store() -> Value {
    json!({
        "name": "Store",
        "fields": [
            { "key": "city", "name": "City", "kind": "text", "required": true },
            { "key": "seats", "name": "Seats", "kind": "integer" },
            { "key": "kind", "name": "Kind", "kind": "choice", "choices": ["shop", "outlet"] }
        ]
    })
}

#[tokio::test]
async fn shops_define_their_own_content() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let ty = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/content-types/store",
        store(),
        200,
    )
    .await;
    assert_eq!(ty["key"], "store");
    assert_eq!(ty["fields"].as_array().unwrap().len(), 3);
    assert_eq!(ty["public"], false);
    assert_eq!(ty["entries"], 0);

    // Bad keys and fields are refused.
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/content-types/Store",
        store(),
        422,
    )
    .await;
    let mut bad = store();
    bad["fields"][1]["kind"] = json!("colour");
    send(&client, &t, "acme", "PUT", "/api/content-types/x", bad, 422).await;

    // Entries fit their fields.
    let e = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/content/store",
        json!({ "fields": { "city": "Berlin Mitte", "seats": 12, "kind": "shop" } }),
        201,
    )
    .await;
    let id = e["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("entry_"));
    assert_eq!(e["handle"], "berlin-mitte");
    assert_eq!(e["content_type"], "store");
    assert_eq!(e["published"], false);
    for (body, code) in [
        (json!({ "fields": { "seats": 3 } }), "invalid_entry"),
        (
            json!({ "fields": { "city": "X", "seats": "3" } }),
            "invalid_entry",
        ),
        (
            json!({ "fields": { "city": "X", "kind": "pop-up" } }),
            "invalid_entry",
        ),
        (
            json!({ "fields": { "city": "X", "colour": "red" } }),
            "invalid_entry",
        ),
    ] {
        let err = send(&client, &t, "acme", "POST", "/api/content/store", body, 422).await;
        assert_eq!(err["code"], code);
    }
    let dup = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/content/store",
        json!({ "handle": "berlin-mitte", "fields": { "city": "Again" } }),
        409,
    )
    .await;
    assert_eq!(dup["code"], "handle_taken");
    send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/content/nope",
        json!({}),
        404,
    )
    .await;

    let list = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/content/store",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["total"], 1);

    // Drafts and private types stay off the storefront.
    public(&client, "acme", "/api/storefront/content/store", 404).await;
    let mut open = store();
    open["public"] = json!(true);
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/content-types/store",
        open.clone(),
        200,
    )
    .await;
    let shown = public(&client, "acme", "/api/storefront/content/store", 200).await;
    assert_eq!(shown["total"], 0);
    let path = format!("/api/content/store/{id}");
    let e = send(
        &client,
        &t,
        "acme",
        "PUT",
        &path,
        json!({ "handle": "berlin", "published": true,
                "fields": { "city": "Berlin", "seats": 14 } }),
        200,
    )
    .await;
    assert_eq!(e["published"], true);
    let shown = public(&client, "acme", "/api/storefront/content/store", 200).await;
    assert_eq!(
        shown["items"][0]["fields"],
        json!({ "city": "Berlin", "seats": 14 })
    );
    let one = public(&client, "acme", "/api/storefront/content/store/berlin", 200).await;
    assert_eq!(one["id"], id);

    // Changing the type keeps entries that fit, drops taken-out fields,
    // and refuses what entries would not fit.
    let mut required = open.clone();
    required["fields"][1]["required"] = json!(true);
    required["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "key": "phone", "name": "Phone", "kind": "text", "required": true }));
    let err = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/content-types/store",
        required,
        409,
    )
    .await;
    assert_eq!(err["code"], "entries_do_not_fit");
    let mut fewer = open.clone();
    fewer["fields"].as_array_mut().unwrap().remove(1);
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/content-types/store",
        fewer,
        200,
    )
    .await;
    let e = send(&client, &t, "acme", "GET", &path, Value::Null, 200).await;
    assert_eq!(e["fields"], json!({ "city": "Berlin" }));

    // Another shop sees none of it.
    send(
        &client,
        &t,
        "globex",
        "GET",
        "/api/content-types/store",
        Value::Null,
        404,
    )
    .await;
    send(&client, &t, "globex", "GET", &path, Value::Null, 404).await;
    send(&client, &t, "globex", "DELETE", &path, Value::Null, 404).await;
    public(&client, "globex", "/api/storefront/content/store", 404).await;
    let types = send(
        &client,
        &t,
        "globex",
        "GET",
        "/api/content-types",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(types, json!([]));

    // A type with entries stays until they are gone.
    let err = send(
        &client,
        &t,
        "acme",
        "DELETE",
        "/api/content-types/store",
        Value::Null,
        409,
    )
    .await;
    assert_eq!(err["code"], "content_type_in_use");
    send(&client, &t, "acme", "DELETE", &path, Value::Null, 204).await;
    send(
        &client,
        &t,
        "acme",
        "DELETE",
        "/api/content-types/store",
        Value::Null,
        204,
    )
    .await;

    // Every change is in the event log.
    let events = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/events?limit=50",
        Value::Null,
        200,
    )
    .await;
    let kinds: Vec<&str> = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    for k in [
        "content_type.defined",
        "content_entry.created",
        "content_entry.updated",
        "content_entry.deleted",
        "content_type.deleted",
    ] {
        assert!(kinds.contains(&k), "{k} in {kinds:?}");
    }
}
