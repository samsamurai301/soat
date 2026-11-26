//! F5: `soat app new` and the dev loop, against the in-process server.

mod common;

use std::path::PathBuf;

use serde_json::{json, Value};
use soat_cli::{install_link, login, new_app, push, Api, ApiFuture, MANIFEST};
use zapline::test::TestClient;

const SERVER: &str = "http://soat.test";

struct InProcess {
    client: TestClient,
    token: Option<String>,
}

impl Api for InProcess {
    fn server(&self) -> &str {
        SERVER
    }

    fn call<'a>(&'a self, method: &'a str, path: &'a str, body: Option<Value>) -> ApiFuture<'a> {
        Box::pin(async move {
            let mut req = match method {
                "POST" => self.client.post(path),
                "PATCH" => self.client.patch(path),
                _ => self.client.get(path),
            };
            if let Some(t) = &self.token {
                req = req.bearer(t);
            }
            if let Some(b) = &body {
                req = req.json(b);
            }
            let res = req.send().await;
            let text = res.text();
            Ok((
                res.status().as_u16(),
                serde_json::from_str(&text).unwrap_or(Value::Null),
            ))
        })
    }
}

fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("soat-app-{nanos}-{}", std::process::id()))
}

#[tokio::test]
async fn developers_start_an_app_and_keep_it_in_step() {
    let (client, _t) = common::with_tenants(&["acme"]).await;
    let token = login(
        &InProcess {
            client: client.clone(),
            token: None,
        },
        common::ADMIN_EMAIL,
        common::ADMIN_PASSWORD,
    )
    .await
    .unwrap();
    let api = InProcess {
        client: client.clone(),
        token: Some(token.clone()),
    };

    // A new app from the template.
    let dir = scratch().join("reviews");
    let files = new_app(&dir, "Reviews").unwrap();
    assert!(files.iter().any(|f| f.ends_with("server.mjs")));
    let manifest = std::fs::read_to_string(dir.join(MANIFEST)).unwrap();
    assert!(manifest.contains("\"name\": \"Reviews\""));
    assert!(new_app(&dir, "Reviews").unwrap_err().contains("not empty"));

    // Not signed in: told how to sign in.
    let anon = InProcess {
        client: client.clone(),
        token: None,
    };
    assert!(push(&anon, &dir).await.unwrap_err().contains("soat login"));

    // The first push registers it and keeps its secret in .env.
    let first = push(&api, &dir).await.unwrap();
    assert!(first.created && first.client_id.starts_with("app_"));
    let env = std::fs::read_to_string(dir.join(".env")).unwrap();
    assert!(env.contains(&format!("SOAT_CLIENT_ID={}", first.client_id)));
    assert!(env.contains("SOAT_CLIENT_SECRET=soat_cs_"));
    assert!(env.contains(&format!("SOAT_URL={SERVER}")));
    let apps = api.call("GET", "/api/apps", None).await.unwrap().1;
    assert_eq!(apps[0]["storefront_blocks"][0]["handle"], "stars");
    assert_eq!(apps[0]["admin_links"][0]["target"], "product");
    assert_eq!(apps[0]["app_url"], "http://localhost:3000/install");

    // Later pushes update it from the manifest.
    let mut m: Value = serde_json::from_str(&manifest).unwrap();
    m["permissions"] = json!(["products:read", "orders:read"]);
    m["tagline"] = json!("Stars under every product");
    std::fs::write(dir.join(MANIFEST), m.to_string()).unwrap();
    let again = push(&api, &dir).await.unwrap();
    assert_eq!(again.client_id, first.client_id);
    assert!(!again.created);
    let apps = api.call("GET", "/api/apps", None).await.unwrap().1;
    assert_eq!(apps.as_array().unwrap().len(), 1);
    assert_eq!(
        apps[0]["permissions"],
        json!(["orders:read", "products:read"])
    );
    assert_eq!(apps[0]["tagline"], "Stars under every product");

    // A manifest the server refuses says why.
    m["permissions"] = json!(["rockets:launch"]);
    std::fs::write(dir.join(MANIFEST), m.to_string()).unwrap();
    assert!(push(&api, &dir).await.unwrap_err().contains("422"));

    // The install link opens the consent screen for the app.
    let link = install_link(SERVER, "acme", &first.client_id, &first.redirect_uri);
    let path = link.strip_prefix(SERVER).unwrap();
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let me = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let page = client.get(path).header("cookie", &me).send().await;
    let html = page.text();
    assert_eq!(page.status(), 200, "{html}");
    assert!(
        html.contains("Install Reviews") && html.contains("orders:read"),
        "{html}"
    );

    std::fs::remove_dir_all(dir.parent().unwrap()).ok();
}
