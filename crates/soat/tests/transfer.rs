//! G4: products and customers in and out as CSV.

mod common;

use serde_json::{json, Value};
use soat_core::csv::Table;
use zapline::test::{MultipartForm, TestClient, TestResponse};

async fn sign_in(client: &TestClient, email: &str, password: &str) -> String {
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": email, "password": password }))
        .send()
        .await;
    format!("soat_admin={}", res.cookie("soat_admin").unwrap().value())
}

async fn upload(client: &TestClient, cookie: &str, path: &str, csv: &str) -> TestResponse {
    client
        .post(path)
        .header("cookie", cookie)
        .multipart(MultipartForm::new().file("file", "data.csv", "text/csv", csv.to_owned()))
        .send()
        .await
}

async fn download(client: &TestClient, cookie: &str, path: &str) -> Table {
    let res = client.get(path).header("cookie", cookie).send().await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("text/csv; charset=utf-8"));
    assert!(res
        .header("content-disposition")
        .unwrap()
        .starts_with("attachment"));
    Table::parse(&res.text()).unwrap()
}

#[tokio::test]
async fn products_round_trip_through_csv() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let api = |method: &str, path: String, body: Option<Value>| {
        let req = match method {
            "PATCH" => client.patch(&path),
            _ => client.get(&path),
        }
        .header("x-soat-tenant", "acme")
        .bearer(&t);
        async move {
            let res = match body {
                Some(b) => req.json(&b).send().await,
                None => req.send().await,
            };
            res.assert_status(200);
            res.json::<Value>()
        }
    };
    api(
        "PATCH",
        "/api/shop".into(),
        Some(json!({ "country": "DE", "currency": "EUR" })),
    )
    .await;
    let me = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let html = client
        .get("/admin/acme/transfer")
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(html.contains("Export products") && html.contains("enctype=\"multipart/form-data\""));

    // As a spreadsheet in Germany saves it: a byte order mark, semicolons,
    // decimal commas.
    let csv = "\u{feff}handle;title;description;status;tags;sku;price;currency;option1_name;option1_value;weight_grams;tax_class;stock\r\n\
        tee;Organic Tee;Soft.;active;tops, organic;TEE-S;29,00;;Size;S;180;;5\r\n\
        tee;;;;;TEE-M;29,00;;;M;;;7\r\n\
        tee;;;;;TEE-L;31,00;;;L;;;0\r\n\
        mug;Mug;\"Holds 300 ml; dishwasher safe\";draft;;MUG;12.50;;;;;reduced;10\r\n\
        bad;;;;;BAD;1;;;;;;\r\n\
        neg;Neg;;;;NEG;x;;;;;;\r\n";
    let res = upload(&client, &me, "/admin/acme/transfer/products", csv).await;
    res.assert_status(422);
    let html = res.text();
    assert!(html.contains("2 created, 0 updated"), "{html}");
    assert!(
        html.contains("Line 6: bad: a new product needs a title"),
        "{html}"
    );
    assert!(
        html.contains("Line 7: neg: the price “x” is not an amount in EUR"),
        "{html}"
    );

    let products = api("GET", "/api/products?sort=title".into(), None).await;
    assert_eq!(products["total"], 2);
    let mug = &products["items"][0];
    assert_eq!(mug["handle"], "mug");
    assert_eq!(mug["status"], "draft");
    assert_eq!(mug["tax_class"], "reduced");
    assert_eq!(mug["price_minor"], 1250);
    assert_eq!(mug["description"], "Holds 300 ml; dishwasher safe");
    let tee = products["items"][1].clone();
    assert_eq!(tee["tags"], json!(["tops", "organic"]));
    let tee_id = tee["id"].as_str().unwrap().to_owned();
    let variants = api("GET", format!("/api/products/{tee_id}/variants"), None).await;
    let skus: Vec<&str> = variants
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["sku"].as_str().unwrap())
        .collect();
    assert_eq!(skus, ["TEE-S", "TEE-M", "TEE-L"]);
    assert_eq!(variants[2]["price_minor"], 3100);
    let m = variants[1]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        api("GET", format!("/api/inventory/{m}"), None).await["on_hand"],
        7
    );

    let table = download(&client, &me, "/admin/acme/transfer/products.csv").await;
    assert_eq!(table.rows.len(), 4);
    let tee_rows: Vec<usize> = (0..4)
        .filter(|r| table.get(*r, "handle") == "tee")
        .collect();
    assert_eq!(table.get(tee_rows[0], "option1_name"), "Size");
    assert_eq!(table.get(tee_rows[0], "title"), "Organic Tee");
    assert_eq!(table.get(tee_rows[1], "title"), "");
    assert_eq!(table.get(tee_rows[1], "stock"), "7");
    assert_eq!(table.get(tee_rows[2], "price"), "31.00");

    // Edit the export and bring it back: a new size, a new price.
    let mut rows = vec![table.headers.clone()];
    rows.extend(table.rows.iter().cloned());
    let price = table.column("price").unwrap();
    let sku = table.column("sku").unwrap();
    let value = table.column("option1_value").unwrap();
    rows[tee_rows[1] + 1][price] = "30.00".into();
    let mut xl = rows[tee_rows[2] + 1].clone();
    xl[sku] = "TEE-XL".into();
    xl[value] = "XL".into();
    rows.push(xl);
    let res = upload(
        &client,
        &me,
        "/admin/acme/transfer/products",
        &soat_core::csv::write(&rows),
    )
    .await;
    res.assert_status(200);
    assert!(res.text().contains("0 created, 2 updated"));
    let variants = api("GET", format!("/api/products/{tee_id}/variants"), None).await;
    assert_eq!(variants.as_array().unwrap().len(), 4);
    assert_eq!(variants[1]["price_minor"], 3000);
    assert_eq!(variants[3]["sku"], "TEE-XL");
    let options = api("GET", format!("/api/products/{tee_id}/options"), None).await;
    assert_eq!(options[0]["values"], json!(["S", "M", "L", "XL"]));
    // Units on hand were counted again, not added.
    assert_eq!(
        api("GET", format!("/api/inventory/{m}"), None).await["on_hand"],
        7
    );

    // Another shop's export holds only its own.
    let table = download(&client, &me, "/admin/globex/transfer/products.csv").await;
    assert!(table.rows.is_empty());

    // Not CSV, or not products.
    let res = upload(
        &client,
        &me,
        "/admin/acme/transfer/products",
        "sku,price\nA,1\n",
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("needs a `handle` column"));
    let res = upload(
        &client,
        &me,
        "/admin/acme/transfer/products",
        "handle\n\"open",
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("never closed"));
}

#[tokio::test]
async fn customers_round_trip_through_csv() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let me = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let csv = "email,first_name,last_name,accepts_marketing,tags\n\
        Ada@Example.com,Ada,Lovelace,yes,\"vip, de\"\n\
        bob@example.com,Bob,,maybe,\n\
        ,No,Email,,\n";
    let res = upload(&client, &me, "/admin/acme/transfer/customers", csv).await;
    res.assert_status(422);
    let html = res.text();
    assert!(html.contains("1 created, 0 updated"), "{html}");
    assert!(
        html.contains("Line 3: bob@example.com: write yes or no"),
        "{html}"
    );
    assert!(html.contains("Line 4: The email is empty."), "{html}");

    let table = download(&client, &me, "/admin/acme/transfer/customers.csv").await;
    assert_eq!(table.rows.len(), 1);
    assert_eq!(table.get(0, "email"), "ada@example.com");
    assert_eq!(table.get(0, "accepts_marketing"), "yes");
    assert_eq!(table.get(0, "tags"), "vip, de");

    let res = upload(
        &client,
        &me,
        "/admin/acme/transfer/customers",
        "email,last_name\nada@example.com,Byron\n",
    )
    .await;
    res.assert_status(200);
    assert!(res.text().contains("0 created, 1 updated"));
    let list = client
        .get("/api/customers")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .json::<Value>();
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["last_name"], "Byron");
    assert_eq!(list["items"][0]["first_name"], "Ada");

    // A role that may only read customers imports nothing.
    client
        .put("/api/roles/viewer")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "permissions": ["customers:read"] }))
        .send()
        .await
        .assert_status(200);
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "email": "viewer@example.com", "password": "a long password", "role": "viewer" }))
        .send()
        .await
        .assert_status(201);
    let viewer = sign_in(&client, "viewer@example.com", "a long password").await;
    let res = upload(
        &client,
        &viewer,
        "/admin/acme/transfer/customers",
        "email\nnew@example.com\n",
    )
    .await;
    res.assert_status(403);
    assert!(res.text().contains("Your role does not allow this."));
}

/// Part of a product export as Shopify writes it.
const SHOPIFY_PRODUCTS: &str = "Handle,Title,Body (HTML),Vendor,Product Category,Type,Tags,Published,Option1 Name,Option1 Value,Option2 Name,Option2 Value,Option3 Name,Option3 Value,Variant SKU,Variant Grams,Variant Inventory Tracker,Variant Inventory Qty,Variant Inventory Policy,Variant Fulfillment Service,Variant Price,Variant Compare At Price,Variant Requires Shipping,Variant Taxable,Variant Barcode,Image Src,Image Position,Image Alt Text,Gift Card,SEO Title,SEO Description,Variant Image,Variant Weight Unit,Variant Tax Code,Cost per item,Status\n\
linen-shirt,Linen Shirt,\"<p>Breathable <strong>linen</strong>.</p><ul><li>Relaxed fit</li><li>Made in Lithuania</li></ul>\",Acme,,Shirts,\"summer, linen\",TRUE,Size,S,Color,White,,,LIN-S-W,220.0,shopify,3,deny,manual,89.00,,TRUE,TRUE,,https://cdn.shopify.com/a.jpg,1,,FALSE,Linen shirt for summer,Breathable linen.,,g,,,active\n\
linen-shirt,,,,,,,,,M,,White,,,LIN-M-W,220.0,shopify,5,deny,manual,89.00,,TRUE,TRUE,,https://cdn.shopify.com/b.jpg,2,,,,,,g,,,\n\
linen-shirt,,,,,,,,,M,,Sand,,,LIN-M-S,220.0,shopify,0,deny,manual,92.00,,TRUE,TRUE,,,,,,,,,g,,,\n\
linen-shirt,,,,,,,,,,,,,,,,,,,,,,,,,https://cdn.shopify.com/c.jpg,3,,,,,,,,,\n\
gift-card,Gift Card,,Acme,,,,TRUE,Title,Default Title,,,,,GIFT,0.0,,0,deny,manual,50.00,,FALSE,FALSE,,,,,TRUE,,,,g,,,draft\n";

#[tokio::test]
async fn shopify_exports_import_as_they_are() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    client
        .patch("/api/shop")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "country": "DE", "currency": "EUR" }))
        .send()
        .await
        .assert_status(200);
    let me = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let res = upload(
        &client,
        &me,
        "/admin/acme/transfer/products",
        SHOPIFY_PRODUCTS,
    )
    .await;
    let html = res.text();
    assert_eq!(res.status(), 200, "{html}");
    assert!(html.contains("2 created, 0 updated"), "{html}");

    let api = |path: String| {
        let req = client.get(&path).header("x-soat-tenant", "acme").bearer(&t);
        async move {
            let res = req.send().await;
            res.assert_status(200);
            res.json::<Value>()
        }
    };
    let products = api("/api/products?sort=title".into()).await;
    let card = &products["items"][0];
    assert_eq!(card["handle"], "gift-card");
    assert_eq!(card["sku"], "GIFT");
    assert_eq!(card["status"], "draft");
    assert_eq!(card["tax_class"], "exempt");
    assert_eq!(card["price_minor"], 5000);
    let shirt = &products["items"][1];
    assert_eq!(
        shirt["description"],
        "Breathable **linen**.\n\n- Relaxed fit\n- Made in Lithuania"
    );
    assert_eq!(shirt["tags"], json!(["summer", "linen"]));
    assert_eq!(shirt["seo_title"], "Linen shirt for summer");
    assert_eq!(shirt["weight_grams"], 220);
    let id = shirt["id"].as_str().unwrap();
    let options = api(format!("/api/products/{id}/options")).await;
    assert_eq!(
        options,
        json!([{ "name": "Size", "values": ["S", "M"] }, { "name": "Color", "values": ["White", "Sand"] }])
    );
    let variants = api(format!("/api/products/{id}/variants")).await;
    assert_eq!(variants.as_array().unwrap().len(), 3);
    assert_eq!(variants[2]["price_minor"], 9200);
    let m = variants[1]["id"].as_str().unwrap();
    assert_eq!(api(format!("/api/inventory/{m}")).await["on_hand"], 5);

    // Problems name the line in Shopify's file.
    let broken = SHOPIFY_PRODUCTS.replace(
        "LIN-M-S,220.0,shopify,0,deny,manual,92.00",
        "LIN-M-S,220.0,shopify,0,deny,manual,lots",
    );
    let res = upload(&client, &me, "/admin/acme/transfer/products", &broken).await;
    res.assert_status(422);
    assert!(
        res.text()
            .contains("Line 2: linen-shirt: line 4: the price “lots”"),
        "{}",
        res.text()
    );

    let customers = "First Name,Last Name,Email,Accepts Email Marketing,Default Address Company,Default Address Address1,Default Address City,Default Address Country Code,Default Address Phone,Phone,Accepts SMS Marketing,Total Spent,Total Orders,Note,Tax Exempt,Tags\n\
        Ada,Lovelace,ada@example.com,yes,,Hauptstr. 1,Berlin,DE,'+493012345,,no,120.00,2,Likes tea,no,\"vip, newsletter\"\n\
        Bob,,bob@example.com,no,,,,,,,no,0.00,0,,no,\n";
    let res = upload(&client, &me, "/admin/acme/transfer/customers", customers).await;
    res.assert_status(200);
    assert!(res.text().contains("2 created, 0 updated"));
    let list = api("/api/customers?sort=email".into()).await;
    assert_eq!(list["items"][0]["phone"], "+493012345");
    assert_eq!(list["items"][0]["accepts_marketing"], true);
    assert_eq!(list["items"][0]["tags"], json!(["vip", "newsletter"]));
    assert_eq!(list["items"][1]["accepts_marketing"], false);
}
