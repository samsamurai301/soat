//! B8: inventory and locations.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

struct Shop<'a> {
    client: &'a TestClient,
    token: &'a str,
    shop: &'a str,
}

impl Shop<'_> {
    async fn send(&self, method: &str, path: &str, body: Value, status: u16) -> Value {
        let req = match method {
            "POST" => self.client.post(path),
            "PATCH" => self.client.patch(path),
            "PUT" => self.client.put(path),
            "DELETE" => self.client.delete(path),
            _ => self.client.get(path),
        }
        .header("x-soat-tenant", self.shop)
        .bearer(self.token);
        let req = if body.is_null() { req } else { req.json(&body) };
        let res = req.send().await;
        let res = res.assert_status(status);
        if status == 204 || status >= 400 {
            Value::Null
        } else {
            res.json()
        }
    }

    async fn location(&self, name: &str) -> String {
        let l = self
            .send(
                "POST",
                "/api/locations",
                json!({ "name": name, "country": "DE" }),
                201,
            )
            .await;
        l["id"].as_str().unwrap().to_owned()
    }

    async fn product(&self, sku: &str) -> String {
        let p = self
            .send(
                "POST",
                "/api/products",
                json!({ "sku": sku, "title": sku, "price_minor": 1, "currency": "EUR" }),
                201,
            )
            .await;
        p["id"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn stock_moves_through_the_ledger() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let s = Shop {
        client: &client,
        token: &token,
        shop: "acme",
    };
    let berlin = s.location("Berlin").await;
    let hamburg = s.location("Hamburg").await;
    let item = s.product("TEE").await;
    let path = format!("/api/inventory/{item}");

    let v = s.send("GET", &path, Value::Null, 200).await;
    assert_eq!(
        (v["on_hand"].as_i64(), v["tracked"].as_bool()),
        (Some(0), Some(true))
    );

    let adjust = |loc: &str, delta: i64, reason: &str| json!({ "location_id": loc, "delta": delta, "reason": reason });
    s.send(
        "POST",
        &format!("{path}/adjustments"),
        adjust(&berlin, 10, "received"),
        200,
    )
    .await;
    s.send(
        "POST",
        &format!("{path}/adjustments"),
        adjust(&berlin, -11, "damaged"),
        409,
    )
    .await;
    s.send(
        "POST",
        &format!("{path}/adjustments"),
        adjust(&berlin, 1, "stolen"),
        422,
    )
    .await;
    s.send(
        "POST",
        &format!("{path}/adjustments"),
        adjust(&berlin, 0, "other"),
        422,
    )
    .await;
    s.send(
        "POST",
        &format!("{path}/adjustments"),
        adjust("loc_nope", 1, "other"),
        422,
    )
    .await;

    let v = s
        .send(
            "POST",
            &format!("{path}/transfers"),
            json!({ "from_location_id": berlin, "to_location_id": hamburg, "quantity": 4 }),
            200,
        )
        .await;
    assert_eq!(v["on_hand"], 10);
    let at = |v: &Value, loc: &str| {
        v["levels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["location_id"] == loc)
            .map(|l| l["on_hand"].as_i64().unwrap())
    };
    assert_eq!((at(&v, &berlin), at(&v, &hamburg)), (Some(6), Some(4)));

    let v = s
        .send(
            "POST",
            &format!("{path}/count"),
            json!({ "location_id": hamburg, "on_hand": 3 }),
            200,
        )
        .await;
    assert_eq!(v["on_hand"], 9);

    let ledger = s
        .send("GET", &format!("{path}/adjustments"), Value::Null, 200)
        .await;
    let reasons: Vec<(&str, i64)> = ledger["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["reason"].as_str().unwrap(), a["delta"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        reasons,
        [
            ("count", -1),
            ("transfer", 4),
            ("transfer", -4),
            ("received", 10)
        ]
    );
    assert!(ledger["items"][0]["actor"]
        .as_str()
        .unwrap()
        .starts_with("user_"));

    // A location with stock cannot go; an empty one can.
    s.send(
        "DELETE",
        &format!("/api/locations/{berlin}"),
        Value::Null,
        409,
    )
    .await;
    let spare = s.location("Spare").await;
    s.send(
        "DELETE",
        &format!("/api/locations/{spare}"),
        Value::Null,
        204,
    )
    .await;

    let low = s
        .send("GET", "/api/inventory?sort=on_hand", Value::Null, 200)
        .await;
    assert_eq!(low["items"][0]["on_hand"], 3);

    let v = s
        .send("PATCH", &path, json!({ "allow_backorder": true }), 200)
        .await;
    assert_eq!(
        (v["tracked"].as_bool(), v["allow_backorder"].as_bool()),
        (Some(true), Some(true))
    );
}

#[tokio::test]
async fn variants_are_stocked_not_their_product() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let s = Shop {
        client: &client,
        token: &token,
        shop: "acme",
    };
    let loc = s.location("Berlin").await;
    let product = s.product("TEE").await;
    s.send(
        "PUT",
        &format!("/api/products/{product}/options"),
        json!({ "options": [{ "name": "Size", "values": ["S", "M"] }] }),
        200,
    )
    .await;
    let v = s
        .send(
            "POST",
            &format!("/api/products/{product}/variants"),
            json!({ "sku": "TEE-S", "options": ["S"] }),
            201,
        )
        .await;
    let variant = v["id"].as_str().unwrap().to_owned();
    s.send(
        "GET",
        &format!("/api/inventory/{product}"),
        Value::Null,
        422,
    )
    .await;
    s.send(
        "POST",
        &format!("/api/inventory/{variant}/adjustments"),
        json!({ "location_id": loc, "delta": 5, "reason": "received" }),
        200,
    )
    .await;

    // Deleting the product drops the variant's stock once events run.
    s.send(
        "DELETE",
        &format!("/api/products/{product}"),
        Value::Null,
        204,
    )
    .await;
    soat_events::dispatch_resources(&client.resources().await)
        .await
        .unwrap();
    let levels = s.send("GET", "/api/inventory", Value::Null, 200).await;
    assert_eq!(levels["total"], 0);
    // The ledger stays.
    let ledger = s
        .send(
            "GET",
            &format!("/api/inventory/{variant}/adjustments"),
            Value::Null,
            200,
        )
        .await;
    assert_eq!(ledger["total"], 1);
}

#[tokio::test]
async fn inventory_stays_in_its_shop() {
    let (client, token) = common::with_tenants(&["acme", "globex"]).await;
    let acme = Shop {
        client: &client,
        token: &token,
        shop: "acme",
    };
    let globex = Shop {
        client: &client,
        token: &token,
        shop: "globex",
    };
    let their_loc = globex.location("Theirs").await;
    let their_item = globex.product("G").await;
    let our_loc = acme.location("Ours").await;
    let our_item = acme.product("A").await;

    acme.send(
        "GET",
        &format!("/api/locations/{their_loc}"),
        Value::Null,
        404,
    )
    .await;
    acme.send(
        "DELETE",
        &format!("/api/locations/{their_loc}"),
        Value::Null,
        404,
    )
    .await;
    acme.send(
        "GET",
        &format!("/api/inventory/{their_item}"),
        Value::Null,
        404,
    )
    .await;
    acme.send(
        "POST",
        &format!("/api/inventory/{their_item}/adjustments"),
        json!({ "location_id": our_loc, "delta": 1, "reason": "received" }),
        404,
    )
    .await;
    // Our item at their location: the location is unknown here.
    acme.send(
        "POST",
        &format!("/api/inventory/{our_item}/adjustments"),
        json!({ "location_id": their_loc, "delta": 1, "reason": "received" }),
        422,
    )
    .await;
    let list = acme.send("GET", "/api/locations", Value::Null, 200).await;
    assert_eq!(list["total"], 1);
}
