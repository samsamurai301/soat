//! The shop API, in process, called with the staff member's session.

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use zapline::db::sqlx::PgPool;
use zapline::db::Db;
use zapline::prelude::*;
use zapline::test::TestClient;

/// The requests behind admin pages: the API itself, in process.
#[derive(Clone)]
pub(crate) struct Api {
    client: TestClient,
    /// The server's database pool, lent to the API on its first request.
    pool: Arc<OnceLock<PgPool>>,
}

impl Api {
    pub fn new(build: impl FnOnce(App) -> App) -> Self {
        let pool: Arc<OnceLock<PgPool>> = Arc::default();
        let lent = pool.clone();
        let app = App::new().on_startup(move |res: Resources| async move {
            let pool = lent
                .get()
                .cloned()
                .ok_or_else(|| Error::internal("the admin started without a database"))?;
            res.insert(pool);
            Ok(())
        });
        Self {
            client: build(app).test(),
            pool,
        }
    }

    /// Runs `method path` with session `token` in shop `shop`. Answers the
    /// status and the body (JSON when it is JSON).
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        db: &Db,
        method: Method,
        path: &str,
        body: Option<&Value>,
        token: Option<&str>,
        shop: Option<&str>,
        conn: ConnInfo,
    ) -> (u16, Value) {
        if path.parse::<Uri>().is_err() {
            return (400, json!({ "error": "not a valid path" }));
        }
        let _ = self.pool.get_or_init(|| db.0.clone());
        let mut req = self.client.request(method, path);
        if let Some(t) = token {
            req = req.bearer(t);
        }
        if let Some(s) = shop {
            req = req.header("x-soat-tenant", s);
        }
        if let Some(addr) = conn.remote_addr {
            req = req.remote_addr(addr);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req.send().await;
        let text = res.text();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (res.status().as_u16(), body)
    }

    /// Like [`Api::send`], with a body as it came, such as a
    /// `multipart/form-data` upload passed through.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_raw(
        &self,
        db: &Db,
        method: Method,
        path: &str,
        body: Bytes,
        content_type: &str,
        token: &str,
        shop: &str,
        conn: ConnInfo,
    ) -> (u16, Value) {
        let _ = self.pool.get_or_init(|| db.0.clone());
        let mut req = self
            .client
            .request(method, path)
            .bearer(token)
            .header("x-soat-tenant", shop)
            .header("content-type", content_type)
            .body(body);
        if let Some(addr) = conn.remote_addr {
            req = req.remote_addr(addr);
        }
        let res = req.send().await;
        let text = res.text();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (res.status().as_u16(), body)
    }
}
