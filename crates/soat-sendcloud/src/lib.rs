//! Sendcloud: one shop account prints labels for many European carriers
//! (DHL, DPD, GLS, PostNL, UPS, ...).
//!
//! A shop adds a carrier account with provider `sendcloud`, its API
//! `public_key` and `secret_key` as credentials, and the Sendcloud
//! `shipping_method_id` to use as a setting. A label is a parcel created
//! with `request_label`; its PDF comes from Sendcloud's label printer
//! endpoint, and canceling cancels the parcel.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use soat_http::{send, Bytes, Full, Method, Request};
use soat_shipping::carriers::{CarrierFuture, CarrierLabel, CarrierProvider, LabelRequest};
use zapline::prelude::*;

/// Sendcloud's API.
pub const API_BASE: &str = "https://panel.sendcloud.sc/api/v2";

/// The Sendcloud adapter.
#[derive(Clone)]
pub struct Sendcloud {
    api_base: String,
}

impl Sendcloud {
    /// Against [`API_BASE`], or `SOAT_SENDCLOUD_API_BASE` when set.
    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("SOAT_SENDCLOUD_API_BASE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| API_BASE.to_owned()),
        )
    }

    /// Against `api_base` (a local fake in tests).
    pub fn new(api_base: &str) -> Self {
        Self {
            api_base: api_base.trim_end_matches('/').to_owned(),
        }
    }

    async fn call(
        &self,
        credentials: &Value,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<soat_http::Answer> {
        let unavailable = |e: String| Error::service_unavailable(format!("Sendcloud: {e}"));
        let basic = STANDARD.encode(format!(
            "{}:{}",
            credentials["public_key"].as_str().unwrap_or_default(),
            credentials["secret_key"].as_str().unwrap_or_default()
        ));
        let mut req = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.api_base))
            .header("authorization", format!("Basic {basic}"))
            .header("user-agent", "SOAT/1");
        let payload = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Bytes::from(b.to_string())
            }
            None => Bytes::new(),
        };
        let req = req
            .body(Full::new(payload))
            .map_err(|e| unavailable(e.to_string()))?;
        let answer = send("Sendcloud", req).await?;
        if answer.status.is_success() {
            return Ok(answer);
        }
        if answer.status.is_server_error() {
            return Err(unavailable(format!("answered {}", answer.status)));
        }
        let json = answer.json();
        let detail = json["error"]["message"]
            .as_str()
            .unwrap_or("the request was refused")
            .to_owned();
        Err(Error::unprocessable(format!("Sendcloud: {detail}")).with_code("carrier_error"))
    }
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().trim().to_owned()
}

/// The parcel Sendcloud is asked for.
fn parcel(req: &LabelRequest<'_>) -> Value {
    let to = req.to;
    let name = format!("{} {}", text(to, "first_name"), text(to, "last_name"))
        .trim()
        .to_owned();
    let company = text(to, "company");
    // Sendcloud needs a weight: 1 kg when the items have none.
    let grams = if req.weight_grams > 0 {
        req.weight_grams
    } else {
        1000
    };
    json!({ "parcel": {
        "name": if name.is_empty() { company.clone() } else { name },
        "company_name": company,
        "address": text(to, "line1"),
        "address_2": text(to, "line2"),
        "city": text(to, "city"),
        "postal_code": text(to, "postal_code"),
        "country": text(to, "country"),
        "telephone": text(to, "phone"),
        "email": req.email,
        "weight": format!("{}.{:03}", grams / 1000, grams % 1000),
        "order_number": req.reference,
        "request_label": true,
        "shipment": { "id": req.settings["shipping_method_id"] },
    }})
}

impl CarrierProvider for Sendcloud {
    fn name(&self) -> &'static str {
        "sendcloud"
    }

    fn label(&self) -> &'static str {
        "Sendcloud"
    }

    fn check(&self, credentials: &Value, settings: &Value) -> Result<()> {
        if text(credentials, "public_key").is_empty() || text(credentials, "secret_key").is_empty()
        {
            return Err(
                Error::unprocessable("Sendcloud needs `public_key` and `secret_key`")
                    .with_code("invalid_credentials"),
            );
        }
        if !settings["shipping_method_id"].is_u64() {
            return Err(Error::unprocessable(
                "Sendcloud needs `shipping_method_id`: the shipping method to print with",
            )
            .with_code("invalid_settings"));
        }
        Ok(())
    }

    fn create_label<'a>(&'a self, req: &'a LabelRequest<'a>) -> CarrierFuture<'a, CarrierLabel> {
        Box::pin(async move {
            let body = parcel(req);
            let created = self
                .call(req.credentials, Method::POST, "/parcels", Some(&body))
                .await?
                .json();
            let p = &created["parcel"];
            let id = p["id"]
                .as_u64()
                .ok_or_else(|| Error::service_unavailable("Sendcloud: no parcel id"))?;
            // The label from our own base URL, never from a link in an answer.
            let pdf = self
                .call(
                    req.credentials,
                    Method::GET,
                    &format!("/labels/label_printer/{id}"),
                    None,
                )
                .await?
                .body;
            Ok(CarrierLabel {
                carrier: p["carrier"]["code"]
                    .as_str()
                    .unwrap_or("sendcloud")
                    .to_owned(),
                tracking_number: text(p, "tracking_number"),
                tracking_url: text(p, "tracking_url"),
                carrier_ref: id.to_string(),
                pdf,
            })
        })
    }

    fn cancel_label<'a>(
        &'a self,
        credentials: &'a Value,
        carrier_ref: &'a str,
    ) -> CarrierFuture<'a, ()> {
        Box::pin(async move {
            let id: u64 = carrier_ref
                .parse()
                .map_err(|_| Error::unprocessable("not a Sendcloud parcel"))?;
            self.call(
                credentials,
                Method::POST,
                &format!("/parcels/{id}/cancel"),
                None,
            )
            .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parcels_carry_the_address_and_weight() {
        let to = json!({ "first_name": "Ada", "last_name": "L", "line1": "Hauptstr. 1",
                         "postal_code": "10115", "city": "Berlin", "country": "DE" });
        let settings = json!({ "shipping_method_id": 8 });
        let creds = json!({});
        let req = LabelRequest {
            credentials: &creds,
            settings: &settings,
            to: &to,
            email: "ada@example.com",
            weight_grams: 1250,
            reference: "1001",
        };
        let p = parcel(&req);
        assert_eq!(p["parcel"]["name"], "Ada L");
        assert_eq!(p["parcel"]["weight"], "1.250");
        assert_eq!(p["parcel"]["shipment"]["id"], 8);
        let light = LabelRequest {
            weight_grams: 0,
            ..req
        };
        assert_eq!(parcel(&light)["parcel"]["weight"], "1.000");
    }

    #[test]
    fn accounts_need_keys_and_a_method() {
        let s = Sendcloud::new(API_BASE);
        let keys = json!({ "public_key": "p", "secret_key": "s" });
        assert!(s.check(&keys, &json!({ "shipping_method_id": 8 })).is_ok());
        assert!(s.check(&keys, &json!({})).is_err());
        assert!(s
            .check(&json!({}), &json!({ "shipping_method_id": 8 }))
            .is_err());
    }
}
