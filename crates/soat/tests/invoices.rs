//! C17: invoices and credit notes.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

#[allow(clippy::too_many_arguments)]
async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    headers: &[(&str, &str)],
    body: Value,
    status: u16,
) -> (Value, String) {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "PUT" => client.put(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match bearer {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = headers.iter().fold(req, |r, (k, v)| r.header(*k, *v));
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    let text = res.text();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    (json, text)
}

struct Shop {
    client: TestClient,
    admin: String,
    tee: String,
    book: String,
    rate: String,
    invoice: String,
}

impl Shop {
    async fn staff(&self, method: &str, path: &str, body: Value, status: u16) -> Value {
        call(
            &self.client,
            "acme",
            method,
            path,
            Some(&self.admin),
            &[],
            body,
            status,
        )
        .await
        .0
    }

    async fn page(&self, path: &str, headers: &[(&str, &str)], status: u16) -> String {
        let bearer = if headers.is_empty() {
            Some(self.admin.as_str())
        } else {
            None
        };
        call(
            &self.client,
            "acme",
            "GET",
            path,
            bearer,
            headers,
            Value::Null,
            status,
        )
        .await
        .1
    }

    /// Places an order for two tees and a book to `address`; returns it
    /// with its token.
    async fn order(&self, address: Value, vat: Option<&str>) -> (Value, String) {
        let (c, _) = call(
            &self.client,
            "acme",
            "POST",
            "/api/storefront/carts",
            None,
            &[],
            json!({ "currency": "EUR" }),
            201,
        )
        .await;
        let id = c["cart"]["id"].as_str().unwrap().to_owned();
        let token = c["token"].as_str().unwrap().to_owned();
        let shopper = |method: &'static str, path: String, body: Value, status: u16| {
            let token = token.clone();
            async move {
                call(
                    &self.client,
                    "acme",
                    method,
                    &path,
                    None,
                    &[("x-soat-cart-token", &token)],
                    body,
                    status,
                )
                .await
                .0
            }
        };
        for (item, q) in [(&self.tee, 2), (&self.book, 1)] {
            shopper(
                "POST",
                format!("/api/storefront/carts/{id}/lines"),
                json!({ "item_id": item, "quantity": q }),
                200,
            )
            .await;
        }
        let path = format!("/api/storefront/carts/{id}/checkout");
        shopper(
            "PATCH",
            path.clone(),
            json!({ "email": "ada@example.com", "shipping_address": address, "vat_number": vat, "shipping_rate_id": self.rate }),
            200,
        )
        .await;
        let done = shopper(
            "POST",
            format!("{path}/complete"),
            json!({ "payment_method_id": self.invoice }),
            201,
        )
        .await;
        (
            done["order"].clone(),
            done["order_token"].as_str().unwrap().to_owned(),
        )
    }

    async fn dispatch(&self) {
        soat_events::dispatch_resources(&self.client.resources().await)
            .await
            .unwrap();
    }

    async fn pay(&self, order: &str) {
        let p = self
            .staff(
                "GET",
                &format!("/api/payments?reference={order}"),
                Value::Null,
                200,
            )
            .await;
        let id = p["items"][0]["id"].as_str().unwrap();
        self.staff(
            "POST",
            &format!("/api/payments/{id}/mark-paid"),
            json!({}),
            200,
        )
        .await;
    }

    /// The order's invoices once there are `n`: they are issued by event
    /// handlers, which the background dispatcher may be the one to run.
    async fn issued(&self, order: &str, n: usize) -> Vec<Value> {
        let mut all = Vec::new();
        for _ in 0..50 {
            self.dispatch().await;
            all = self.invoices(order).await;
            if all.len() >= n {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        all
    }

    async fn invoices(&self, order: &str) -> Vec<Value> {
        let page = self
            .staff(
                "GET",
                &format!("/api/invoices?order_id={order}&sort=id"),
                Value::Null,
                200,
            )
            .await;
        page["items"].as_array().unwrap().clone()
    }

    async fn settings(&self, issue_on: &str) {
        self.staff(
            "PUT",
            "/api/invoices/settings",
            json!({
                "legal_name": "Acme GmbH",
                "address": { "line1": "Torstr. 5", "postal_code": "10119", "city": "Berlin", "country": "DE" },
                "vat_number": "DE 123456789",
                "tax_number": "30/123/45678",
                "bank_details": "IBAN DE02 1203 0000 0000 2020 51",
                "footer": "Amtsgericht Charlottenburg HRB 12345",
                "issue_on": issue_on,
            }),
            200,
        )
        .await;
    }
}

fn berlin() -> Value {
    json!({ "first_name": "Ada", "last_name": "Lovelace", "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" })
}

/// A German shop, prices including VAT: an 11.90 tee (19%), a 10.70 book
/// (7%), 4.95 shipping to Germany and Austria, invoice payment.
async fn shop() -> Shop {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let mut s = Shop {
        client,
        admin,
        tee: String::new(),
        book: String::new(),
        rate: String::new(),
        invoice: String::new(),
    };
    let loc = s
        .staff(
            "POST",
            "/api/locations",
            json!({ "name": "W", "country": "DE" }),
            201,
        )
        .await["id"]
        .clone();
    for (sku, price, class) in [("TEE", 1190, "standard"), ("BOOK", 1070, "reduced")] {
        let p = s
            .staff(
                "POST",
                "/api/products",
                json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR", "tax_class": class }),
                201,
            )
            .await;
        let id = p["id"].as_str().unwrap().to_owned();
        s.staff(
            "POST",
            &format!("/api/inventory/{id}/adjustments"),
            json!({ "location_id": loc, "delta": 20, "reason": "received" }),
            200,
        )
        .await;
        if sku == "TEE" {
            s.tee = id;
        } else {
            s.book = id;
        }
    }
    s.staff(
        "PUT",
        "/api/tax/settings",
        json!({ "origin_country": "DE", "prices_include_tax": true, "charge_destination_vat": false }),
        200,
    )
    .await;
    s.staff("POST", "/api/tax/presets/eu-vat", Value::Null, 201)
        .await;
    let z = s
        .staff(
            "POST",
            "/api/shipping/zones",
            json!({ "name": "DACH", "countries": ["DE", "AT"] }),
            201,
        )
        .await;
    let r = s
        .staff(
            "POST",
            &format!("/api/shipping/zones/{}/rates", z["id"].as_str().unwrap()),
            json!({ "name": "Standard", "currency": "EUR", "price_minor": 495 }),
            201,
        )
        .await;
    s.rate = r["id"].as_str().unwrap().to_owned();
    let m = s
        .staff(
            "POST",
            "/api/payment-methods",
            json!({ "provider": "manual", "name": "Invoice", "settings": { "instructions": "Pay in 14 days" } }),
            201,
        )
        .await;
    s.invoice = m["id"].as_str().unwrap().to_owned();
    s
}

#[tokio::test]
async fn paid_orders_are_invoiced_and_refunds_credited() {
    let s = shop().await;
    s.staff("GET", "/api/invoices/settings", Value::Null, 404)
        .await;
    s.settings("paid").await;
    let (order, token) = s.order(berlin(), None).await;
    let id = order["id"].as_str().unwrap();
    s.dispatch().await;
    assert!(s.invoices(id).await.is_empty(), "not before payment");

    s.pay(id).await;
    let all = s.issued(id, 1).await;
    assert_eq!(all.len(), 1);
    let inv = &all[0];
    let number = inv["number"].as_str().unwrap();
    assert!(
        number.starts_with("INV-20") && number.ends_with("-00001"),
        "{number}"
    );
    assert_eq!(inv["kind"], "invoice");
    assert_eq!(inv["order_number"], 1001);
    assert_eq!(inv["seller"]["name"], "Acme GmbH");
    assert_eq!(inv["seller"]["vat_number"], "DE123456789");
    assert_eq!(inv["buyer"]["name"], "Ada Lovelace");
    assert_eq!(inv["lines"].as_array().unwrap().len(), 3);
    assert_eq!(inv["lines"][2]["description"], "Shipping: Standard");
    assert_eq!(inv["lines"][2]["rate_bps"], 1900);
    assert_eq!(
        inv["rates"],
        json!([
            { "rate_bps": 1900, "net_minor": 2416, "tax_minor": 459, "gross_minor": 2875 },
            { "rate_bps": 700, "net_minor": 1000, "tax_minor": 70, "gross_minor": 1070 },
        ])
    );
    assert_eq!(inv["total_minor"], 3945);
    assert_eq!(inv["tax_minor"], 529);
    assert_eq!(inv["net_minor"], 3416);
    let inv_id = inv["id"].as_str().unwrap();

    // A refund credits the invoice by itself, once.
    s.staff(
        "POST",
        &format!("/api/orders/{id}/refunds"),
        json!({ "amount_minor": 1000, "reason": "Damaged" }),
        201,
    )
    .await;
    let all = s.issued(id, 2).await;
    assert_eq!(all.len(), 2);
    let note = &all[1];
    assert_eq!(note["kind"], "credit_note");
    assert!(note["number"].as_str().unwrap().ends_with("-00001"));
    assert!(note["number"].as_str().unwrap().starts_with("CN-"));
    assert_eq!(note["credits_id"], inv_id);
    assert_eq!(note["total_minor"], 1000);
    assert_eq!(note["rates"][0]["gross_minor"], 729);
    assert_eq!(note["rates"][0]["tax_minor"], 116);
    assert_eq!(note["rates"][1]["tax_minor"], 18);
    assert_eq!(
        note["lines"][0]["description"],
        format!("Credit for invoice {number}: Damaged")
    );
    assert_eq!(all[0]["credited_minor"], 1000);

    // Staff credit the rest; nothing is left after.
    let path = format!("/api/invoices/{inv_id}/credit-notes");
    s.staff("POST", &path, json!({ "amount_minor": 2946 }), 422)
        .await;
    let rest = s
        .staff("POST", &path, json!({ "reason": "Order canceled" }), 201)
        .await;
    assert_eq!(rest["total_minor"], 2945);
    assert!(rest["number"].as_str().unwrap().ends_with("-00002"));
    s.staff("POST", &path, json!({}), 422).await;
    let credit = all[1]["id"].as_str().unwrap();
    s.staff(
        "POST",
        &format!("/api/invoices/{credit}/credit-notes"),
        json!({}),
        404,
    )
    .await;

    // Printable, for staff and for the buyer holding the order's token.
    let html = s
        .page(&format!("/api/invoices/{inv_id}/document"), &[], 200)
        .await;
    assert!(html.contains(&format!("Invoice {number}")), "{html}");
    assert!(html.contains("VAT ID DE123456789"));
    assert!(html.contains("<td>19%</td>"));
    assert!(html.contains("39.45 EUR"));
    assert!(html.contains("HRB 12345"));
    let mine = s
        .page(
            &format!("/api/storefront/orders/{id}/invoices"),
            &[("x-soat-order-token", &token)],
            200,
        )
        .await;
    let mine: Value = serde_json::from_str(&mine).unwrap();
    assert_eq!(mine.as_array().unwrap().len(), 3);
    let doc = s
        .page(
            &format!("/api/storefront/orders/{id}/invoices/{credit}/document"),
            &[("x-soat-order-token", &token)],
            200,
        )
        .await;
    assert!(doc.contains(&format!("Credits invoice {number}")), "{doc}");
    s.page(
        &format!("/api/storefront/orders/{id}/invoices"),
        &[("x-soat-order-token", "soat_order_wrong")],
        404,
    )
    .await;
}

#[tokio::test]
async fn manual_issue_and_reverse_charge() {
    let s = shop().await;
    let (order, _) = s.order(berlin(), None).await;
    let id = order["id"].as_str().unwrap();
    s.staff(
        "POST",
        &format!("/api/orders/{id}/invoice"),
        Value::Null,
        422,
    )
    .await;
    s.settings("manual").await;
    s.pay(id).await;
    s.dispatch().await;
    assert!(s.invoices(id).await.is_empty());
    s.staff(
        "POST",
        &format!("/api/orders/{id}/invoice"),
        Value::Null,
        201,
    )
    .await;
    s.staff(
        "POST",
        &format!("/api/orders/{id}/invoice"),
        Value::Null,
        409,
    )
    .await;

    // An Austrian business with a VAT number: no German VAT, and the note.
    let vienna = json!({ "company": "Wien AG", "line1": "Ring 1", "postal_code": "1010", "city": "Wien", "country": "AT" });
    let (order, _) = s.order(vienna, Some("ATU12345678")).await;
    let id = order["id"].as_str().unwrap();
    let inv = s
        .staff(
            "POST",
            &format!("/api/orders/{id}/invoice"),
            Value::Null,
            201,
        )
        .await;
    assert!(inv["number"].as_str().unwrap().ends_with("-00002"));
    assert_eq!(inv["reverse_charge"], true);
    assert_eq!(inv["tax_minor"], 0);
    assert_eq!(inv["buyer"]["name"], "Wien AG");
    assert_eq!(inv["buyer"]["vat_number"], "ATU12345678");
    assert!(inv["note"].as_str().unwrap().starts_with("Reverse charge"));
    assert_eq!(inv["total_minor"], order["total_minor"]);

    s.staff(
        "PUT",
        "/api/invoices/settings",
        json!({ "legal_name": "X", "address": berlin(), "issue_on": "weekly" }),
        422,
    )
    .await;
    s.staff(
        "PUT",
        "/api/invoices/settings",
        json!({ "legal_name": "X", "address": berlin(), "invoice_prefix": "<b>" }),
        422,
    )
    .await;
}

#[tokio::test]
async fn invoices_stay_in_their_shop() {
    let s = shop().await;
    s.settings("placed").await;
    let (order, _) = s.order(berlin(), None).await;
    let all = s.issued(order["id"].as_str().unwrap(), 1).await;
    assert_eq!(all.len(), 1, "issued when placed");
    let id = all[0]["id"].as_str().unwrap();
    let other = |method: &'static str, path: String, status: u16| {
        let s = &s;
        async move {
            call(
                &s.client,
                "other",
                method,
                &path,
                Some(&s.admin),
                &[],
                if method == "POST" {
                    json!({})
                } else {
                    Value::Null
                },
                status,
            )
            .await
            .0
        }
    };
    other("GET", format!("/api/invoices/{id}"), 404).await;
    other("GET", format!("/api/invoices/{id}/document"), 404).await;
    other("POST", format!("/api/invoices/{id}/credit-notes"), 404).await;
    other(
        "POST",
        format!("/api/orders/{}/invoice", order["id"].as_str().unwrap()),
        404,
    )
    .await;
    let list = other("GET", "/api/invoices".into(), 200).await;
    assert!(list["items"].as_array().unwrap().is_empty());
    other("GET", "/api/invoices/settings".into(), 404).await;
    call(
        &s.client,
        "acme",
        "GET",
        "/api/invoices",
        None,
        &[],
        Value::Null,
        401,
    )
    .await;
}

/// GET `path` as staff (or with `headers` instead): status, content type,
/// content disposition and body.
async fn fetch(s: &Shop, path: &str, headers: &[(&str, &str)]) -> (u16, String, String, String) {
    let req = s.client.get(path).header("x-soat-tenant", "acme");
    let req = if headers.is_empty() {
        req.bearer(&s.admin)
    } else {
        headers.iter().fold(req, |r, (k, v)| r.header(*k, *v))
    };
    let res = req.send().await;
    (
        res.status().as_u16(),
        res.header("content-type").unwrap_or_default().to_owned(),
        res.header("content-disposition")
            .unwrap_or_default()
            .to_owned(),
        res.text(),
    )
}

fn rules(check: &Value) -> Vec<String> {
    check["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["rule"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn e_invoices_in_xrechnung_zugferd_and_peppol() {
    let s = shop().await;
    s.settings("paid").await;
    let (order, token) = s.order(berlin(), None).await;
    let id = order["id"].as_str().unwrap();
    s.pay(id).await;
    let inv = s.issued(id, 1).await[0].clone();
    let inv_id = inv["id"].as_str().unwrap();
    let number = inv["number"].as_str().unwrap();

    // The shop has no phone or email yet: XRechnung and Peppol say so.
    let check = |f: &str| format!("/api/invoices/{inv_id}/e-invoice/check?format={f}");
    let x = s.staff("GET", &check("xrechnung"), Value::Null, 200).await;
    assert_eq!(x["ready"], false);
    assert_eq!(
        rules(&x),
        ["PEPPOL-EN16931-R020", "BR-DE-6", "BR-DE-7"],
        "{x}"
    );
    let cii = s.staff("GET", &check("zugferd"), Value::Null, 200).await;
    assert_eq!(cii["format"], "cii");
    assert_eq!(cii["ready"], true, "{cii}");
    // The PDF holds the CII XML, so it is checked as that.
    let pdf = s.staff("GET", &check("pdf"), Value::Null, 200).await;
    assert_eq!(pdf["format"], "cii");
    s.staff("GET", &check("docx"), Value::Null, 422).await;

    s.staff(
        "PUT",
        "/api/invoices/settings",
        json!({
            "legal_name": "Acme GmbH",
            "address": { "line1": "Torstr. 5", "postal_code": "10119", "city": "Berlin", "country": "DE", "phone": "+49 30 1234567" },
            "vat_number": "DE 123456789",
            "email": "billing@acme.example",
            "bank_details": "Acme Bank\nIBAN DE02 1203 0000 0000 2020 51",
            "issue_on": "paid",
        }),
        200,
    )
    .await;
    // The issued invoice keeps its seller; only new invoices get the phone.
    let x = s.staff("GET", &check("xrechnung"), Value::Null, 200).await;
    assert_eq!(rules(&x), ["PEPPOL-EN16931-R020", "BR-DE-6", "BR-DE-7"]);
    let (order, token2) = s.order(berlin(), None).await;
    let id2 = order["id"].as_str().unwrap();
    s.pay(id2).await;
    let inv2 = s.issued(id2, 1).await[0].clone();
    let inv2_id = inv2["id"].as_str().unwrap();
    for f in ["xrechnung", "cii", "peppol"] {
        let c = s
            .staff(
                "GET",
                &format!("/api/invoices/{inv2_id}/e-invoice/check?format={f}"),
                Value::Null,
                200,
            )
            .await;
        assert_eq!(c["ready"], true, "{f}: {c}");
    }

    let (status, kind, disposition, xml) = fetch(
        &s,
        &format!("/api/invoices/{inv2_id}/e-invoice?buyer_reference=04011000-1234512345-06"),
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert!(kind.starts_with("application/xml"));
    assert!(disposition.contains("-xrechnung.xml"), "{disposition}");
    for part in [
        "urn:xeinkauf.de:kosit:xrechnung_3.0",
        "<ram:TypeCode>380</ram:TypeCode>",
        "<ram:BuyerReference>04011000-1234512345-06</ram:BuyerReference>",
        "<ram:CompleteNumber>+49 30 1234567</ram:CompleteNumber>",
        "<ram:URIID schemeID=\"EM\">billing@acme.example</ram:URIID>",
        "<ram:URIID schemeID=\"EM\">ada@example.com</ram:URIID>",
        "<ram:ID schemeID=\"VA\">DE123456789</ram:ID>",
        "<ram:IBANID>DE02120300000000202051</ram:IBANID>",
        "<ram:IssuerAssignedID>1002</ram:IssuerAssignedID>",
        "<ram:CalculatedAmount>4.59</ram:CalculatedAmount>",
        "<ram:BasisAmount>24.16</ram:BasisAmount>",
        "<ram:RateApplicablePercent>7.00</ram:RateApplicablePercent>",
        "<ram:TaxTotalAmount currencyID=\"EUR\">5.29</ram:TaxTotalAmount>",
        "<ram:GrandTotalAmount>39.45</ram:GrandTotalAmount>",
        "<ram:TotalPrepaidAmount>39.45</ram:TotalPrepaidAmount>",
        "<ram:DuePayableAmount>0.00</ram:DuePayableAmount>",
    ] {
        assert!(xml.contains(part), "{part} missing in\n{xml}");
    }
    // Without a reference, the order number.
    let (_, _, _, xml) = fetch(
        &s,
        &format!("/api/invoices/{inv2_id}/e-invoice?format=cii"),
        &[],
    )
    .await;
    assert!(xml.contains("<ram:BuyerReference>1002</ram:BuyerReference>"));
    assert!(xml.contains("<ram:ID>urn:cen.eu:en16931:2017</ram:ID>"));

    // A refund's credit note, in UBL.
    s.staff(
        "POST",
        &format!("/api/orders/{id2}/refunds"),
        json!({ "amount_minor": 1000 }),
        201,
    )
    .await;
    let all = s.issued(id2, 2).await;
    let note = all[1]["id"].as_str().unwrap();
    let (status, _, disposition, ubl) = fetch(
        &s,
        &format!("/api/invoices/{note}/e-invoice?format=peppol"),
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert!(disposition.contains("CN-"), "{disposition}");
    for part in [
        "<CreditNote xmlns=\"urn:oasis:names:specification:ubl:schema:xsd:CreditNote-2\"",
        "<cbc:CreditNoteTypeCode>381</cbc:CreditNoteTypeCode>",
        &format!("<cbc:ID>{}</cbc:ID>", inv2["number"].as_str().unwrap()),
        "<cbc:PayableAmount currencyID=\"EUR\">0.00</cbc:PayableAmount>",
        "<cbc:TaxInclusiveAmount currencyID=\"EUR\">10.00</cbc:TaxInclusiveAmount>",
    ] {
        assert!(ubl.contains(part), "{part} missing in\n{ubl}");
    }

    // The buyer gets theirs with the order's token, not another order's.
    let mine = format!("/api/storefront/orders/{id}/invoices/{inv_id}/e-invoice?format=cii");
    let (status, _, _, xml) = fetch(&s, &mine, &[("x-soat-order-token", &token)]).await;
    assert_eq!(status, 200);
    assert!(xml.contains(&format!("<ram:ID>{number}</ram:ID>")));
    let (status, ..) = fetch(&s, &mine, &[("x-soat-order-token", &token2)]).await;
    assert_eq!(status, 404);
    let theirs = format!("/api/storefront/orders/{id2}/invoices/{inv_id}/e-invoice");
    let (status, ..) = fetch(&s, &theirs, &[("x-soat-order-token", &token2)]).await;
    assert_eq!(status, 404);

    // Another shop sees none of it.
    for path in [
        format!("/api/invoices/{inv_id}/e-invoice"),
        format!("/api/invoices/{inv_id}/e-invoice/check"),
    ] {
        call(
            &s.client,
            "other",
            "GET",
            &path,
            Some(&s.admin),
            &[],
            Value::Null,
            404,
        )
        .await;
    }

    // In the admin: the order lists them, with downloads that work.
    let res = s
        .client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let admin = |path: String| {
        let cookie = cookie.clone();
        let s = &s;
        async move {
            let res = s.client.get(&path).header("cookie", &cookie).send().await;
            (res.status().as_u16(), res.text())
        }
    };
    let (status, page) = admin(format!("/admin/acme/orders/{id}")).await;
    assert_eq!(status, 200);
    assert!(page.contains(number), "{page}");
    assert!(
        page.contains("XRechnung BR-DE-6"),
        "the old invoice lacks a phone"
    );
    let (status, xml) = admin(format!(
        "/admin/acme/invoices/{inv2_id}/e-invoice?format=peppol"
    ))
    .await;
    assert_eq!(status, 200);
    assert!(xml.contains("<cbc:InvoiceTypeCode>380</cbc:InvoiceTypeCode>"));
    let (status, html) = admin(format!("/admin/acme/invoices/{inv2_id}/document")).await;
    assert_eq!(status, 200);
    assert!(html.contains("Invoice "));
}

#[tokio::test]
async fn zugferd_pdfs_carry_the_xml_inside() {
    use std::io::Read as _;

    let s = shop().await;
    s.settings("paid").await;
    let (order, token) = s.order(berlin(), None).await;
    let id = order["id"].as_str().unwrap();
    s.pay(id).await;
    let inv = s.issued(id, 1).await[0].clone();
    let inv_id = inv["id"].as_str().unwrap();
    let number = inv["number"].as_str().unwrap();

    let res = s
        .client
        .get(&format!("/api/invoices/{inv_id}/e-invoice?format=pdf"))
        .header("x-soat-tenant", "acme")
        .bearer(&s.admin)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("application/pdf"));
    assert_eq!(
        res.header("content-disposition"),
        Some(format!("attachment; filename=\"{number}.pdf\"").as_str())
    );
    let pdf = res.bytes().to_vec();
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.starts_with("%PDF-1.7"));
    for part in [
        "<pdfaid:part>3</pdfaid:part>",
        "/AFRelationship /Alternative",
        "/EmbeddedFiles << /Names [(factur-x.xml)",
        "<fx:ConformanceLevel>EN 16931</fx:ConformanceLevel>",
    ] {
        assert!(text.contains(part), "{part}");
    }

    // The attached XML is the invoice's CII, byte for byte.
    let (_, _, _, cii) = fetch(
        &s,
        &format!("/api/invoices/{inv_id}/e-invoice?format=cii"),
        &[],
    )
    .await;
    // Byte offsets: the lossy text's offsets drift past binary streams.
    let find = |needle: &[u8], from: usize| {
        pdf[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|i| from + i)
    };
    let at = find(b"/Type /EmbeddedFile", 0).expect("an embedded file");
    let start = find(b"stream\n", at).unwrap() + b"stream\n".len();
    let end = find(b"\nendstream", start).unwrap();
    let mut xml = String::new();
    flate2::read::ZlibDecoder::new(&pdf[start..end])
        .read_to_string(&mut xml)
        .unwrap();
    assert_eq!(xml, cii);

    // The buyer gets it with the order's token; staff through the admin.
    let mine = format!("/api/storefront/orders/{id}/invoices/{inv_id}/e-invoice?format=pdf");
    let (status, content_type, ..) = fetch(&s, &mine, &[("x-soat-order-token", &token)]).await;
    assert_eq!((status, content_type.as_str()), (200, "application/pdf"));
    let res = s
        .client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let staff = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let res = s
        .client
        .get(&format!(
            "/admin/acme/invoices/{inv_id}/e-invoice?format=pdf"
        ))
        .header("cookie", &staff)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("application/pdf"));
    assert_eq!(res.bytes().to_vec(), pdf);
    let order_page = s
        .client
        .get(&format!("/admin/acme/orders/{id}"))
        .header("cookie", &staff)
        .send()
        .await
        .text();
    assert!(
        order_page.contains("e-invoice?format=pdf"),
        "the order links it"
    );
}

#[cfg(feature = "peppol")]
#[tokio::test]
async fn invoices_go_over_peppol_through_the_shops_access_point() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const SECRET: &str = "the access point bridge's secret";
    const SENDER: &str = "0208:0123456749";
    const BUYER: &str = "0208:0987654394";
    let down = Arc::new(AtomicBool::new(true));
    let flag = down.clone();
    let (base, log) = soat_testing::fake_api(move |r| {
        let signed = r
            .header("soat-signature")
            .is_some_and(|s| soat_webhooks::verify(SECRET, s, r.body.as_bytes()));
        if !signed {
            return (401, json!({ "error": "unsigned" }));
        }
        if flag.load(Ordering::SeqCst) {
            return (503, json!({ "error": "down for maintenance" }));
        }
        (202, json!({ "id": "ap-msg-1" }))
    })
    .await;

    let s = shop().await;
    s.settings("paid").await;
    let (order, _) = s.order(berlin(), None).await;
    let id = order["id"].as_str().unwrap();
    s.pay(id).await;
    let inv = s.issued(id, 1).await[0].clone();
    let inv_id = inv["id"].as_str().unwrap();
    let send = format!("/api/invoices/{inv_id}/peppol");

    // Nothing goes out before the shop chooses an access point.
    let e = s
        .staff("POST", &send, json!({ "receiver_id": BUYER }), 409)
        .await;
    assert_eq!(e["code"], "no_access_point");
    s.staff("GET", "/api/peppol/access-point", Value::Null, 404)
        .await;
    let kinds = s
        .staff("GET", "/api/peppol/access-points", Value::Null, 200)
        .await;
    assert_eq!(kinds[0]["name"], "http");
    let url = format!("{base}/send");
    for (sender, code) in [
        ("0208:0123456789", "invalid_peppol_id"),
        ("DE123", "invalid_peppol_id"),
    ] {
        let e = s
            .staff(
                "PUT",
                "/api/peppol/access-point",
                json!({ "provider": "http", "sender_id": sender,
                        "settings": { "url": url }, "credentials": { "secret": SECRET } }),
                422,
            )
            .await;
        assert_eq!(e["code"], code, "{sender}");
    }
    let ap = s
        .staff(
            "PUT",
            "/api/peppol/access-point",
            json!({ "provider": "http", "sender_id": SENDER,
                    "settings": { "url": url }, "credentials": { "secret": SECRET } }),
            200,
        )
        .await;
    assert_eq!(ap["sender_id"], SENDER);
    assert!(!ap.to_string().contains(SECRET));

    // A bad id is refused before anything is sent.
    let e = s
        .staff("POST", &send, json!({ "receiver_id": "0208:1" }), 422)
        .await;
    assert_eq!(e["code"], "invalid_peppol_id");
    assert!(log.lock().unwrap().is_empty());

    // The access point is down: the try is kept, with why.
    let e = s
        .staff("POST", &send, json!({ "receiver_id": BUYER }), 502)
        .await;
    assert_eq!(e["code"], "access_point_failed");
    let tries = s.staff("GET", &send, Value::Null, 200).await;
    assert_eq!(tries.as_array().unwrap().len(), 1);
    assert_eq!(tries[0]["status"], "failed");
    assert!(tries[0]["error"]
        .as_str()
        .unwrap()
        .contains("down for maintenance"));

    // Up again: it goes, as Peppol BIS with both Peppol ids.
    down.store(false, Ordering::SeqCst);
    let sent = s
        .staff("POST", &send, json!({ "receiver_id": BUYER }), 201)
        .await;
    assert_eq!(sent["status"], "sent");
    assert_eq!(sent["reference"], "ap-msg-1");
    assert_eq!(sent["receiver_id"], BUYER);
    let body = log.lock().unwrap().last().unwrap().json();
    assert_eq!(body["sender"], "iso6523-actorid-upis::0208:0123456749");
    assert_eq!(body["receiver"], "iso6523-actorid-upis::0208:0987654394");
    assert_eq!(body["number"], inv["number"]);
    assert!(body["document_type"].as_str().unwrap().starts_with(
        "busdox-docid-qns::urn:oasis:names:specification:ubl:schema:xsd:Invoice-2::Invoice##"
    ));
    assert_eq!(
        body["process"],
        "cenbii-procid-ubl::urn:fdc:peppol.eu:2017:poacc:billing:01:1.0"
    );
    let xml = body["document"].as_str().unwrap();
    assert!(xml.contains("<cbc:EndpointID schemeID=\"0208\">0123456749</cbc:EndpointID>"));
    assert!(xml.contains("<cbc:EndpointID schemeID=\"0208\">0987654394</cbc:EndpointID>"));

    // Once per buyer; the tries are listed newest first.
    let e = s
        .staff("POST", &send, json!({ "receiver_id": BUYER }), 409)
        .await;
    assert_eq!(e["code"], "already_sent");
    let tries = s.staff("GET", &send, Value::Null, 200).await;
    let statuses: Vec<&str> = tries
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["sent", "failed"]);
    let events = s
        .staff(
            "GET",
            "/api/events?kind=invoice.peppol_sent",
            Value::Null,
            200,
        )
        .await;
    assert_eq!(events["items"].as_array().unwrap().len(), 1);
    assert!(!events.to_string().contains(SECRET));

    // Another shop sees neither the invoice nor its tries.
    let (other, _) = call(
        &s.client,
        "other",
        "GET",
        &send,
        Some(&s.admin),
        &[],
        Value::Null,
        404,
    )
    .await;
    assert!(other.to_string().len() < 200);
    call(
        &s.client,
        "other",
        "POST",
        &send,
        Some(&s.admin),
        &[],
        json!({ "receiver_id": BUYER }),
        404,
    )
    .await;

    // The admin's order page lists the tries and sends.
    let res = s
        .client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let staff = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let page = s
        .client
        .get(&format!("/admin/acme/orders/{id}"))
        .header("cookie", &staff)
        .send()
        .await
        .assert_status(200)
        .text();
    assert!(page.contains("Send over Peppol"), "{page}");
    assert!(page.contains("sent (ap-msg-1)"));
    assert!(page.contains("failed: "));
}
