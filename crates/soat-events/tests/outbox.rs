//! The outbox end to end: publish in a tenant transaction, fan out to
//! subscribers, retry failures with backoff, never deliver rolled-back events.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use soat_auth::Auth;
use soat_events::{dispatch_resources, publish, Events};
use soat_module::{Event, HandlerFuture, Module, Modules, Subscriber};
use soat_tenancy::{Tenancy, Tenant};
use zapline::db::sqlx::{self, Connection, PgConnection};
use zapline::db::{Database, Db};
use zapline::prelude::*;
use zapline::test::TestClient;
use zapline::Resources;

/// Records what it receives; fails the first `fail_first` calls.
struct Recorder {
    name: &'static str,
    only: Option<&'static str>,
    fail_first: usize,
    calls: AtomicUsize,
    seen: Mutex<Vec<Event>>,
}

impl Recorder {
    fn new(name: &'static str, only: Option<&'static str>, fail_first: usize) -> Arc<Self> {
        Arc::new(Self {
            name,
            only,
            fail_first,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn kinds(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect()
    }
}

impl Subscriber for Recorder {
    fn name(&self) -> &'static str {
        self.name
    }
    fn wants(&self, kind: &str) -> bool {
        self.only.is_none_or(|k| k == kind)
    }
    fn handle<'a>(&'a self, event: &'a Event, _: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) < self.fail_first {
                return Err(Error::internal("not yet"));
            }
            self.seen.lock().unwrap().push(event.clone());
            Ok(())
        })
    }
}

struct Wrapper(Arc<Recorder>);

impl Subscriber for Wrapper {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn wants(&self, kind: &str) -> bool {
        self.0.wants(kind)
    }
    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        self.0.handle(event, res)
    }
}

/// A module whose route publishes the posted event, and rolls back when
/// asked to.
struct Probe(Vec<Arc<Recorder>>);

#[derive(serde::Deserialize)]
struct Publish {
    kind: String,
    rollback: bool,
}

async fn publish_route(db: Db, tenant: Tenant, Json(p): Json<Publish>) -> Result<StatusCode> {
    let mut tx = tenant.begin(&db).await?;
    publish(&mut tx, tenant.id(), &p.kind, &json!({ "n": 1 })).await?;
    if p.rollback {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

impl Module for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }
    fn depends_on(&self) -> &'static [&'static str] {
        &["events"]
    }
    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        self.0
            .iter()
            .map(|r| Arc::new(Wrapper(r.clone())) as Arc<dyn Subscriber>)
            .collect()
    }
    fn mount(&self, app: App) -> App {
        app.post("/probe", publish_route)
    }
}

async fn app(recorders: Vec<Arc<Recorder>>) -> (TestClient, String, String) {
    let url = soat_testing::fresh_database_url().await;
    let modules = Modules::new()
        .with(Auth::with_setup_token("t"))
        .with(Tenancy)
        .with(Events::manual())
        .with(Probe(recorders));
    let client = modules
        .build(App::new(), Database::postgres(url.clone()))
        .test();
    let session: Value = client
        .post("/api/setup")
        .json(&json!({ "setup_token": "t", "email": "a@example.com", "password": "admin password 1" }))
        .send()
        .await
        .assert_status(201)
        .json();
    let token = session["token"].as_str().unwrap().to_owned();
    client
        .post("/api/tenants")
        .bearer(&token)
        .json(&json!({ "id": "acme", "name": "Acme" }))
        .send()
        .await
        .assert_status(201);
    (client, token, url)
}

async fn emit(client: &TestClient, token: &str, kind: &str, rollback: bool) {
    client
        .post("/probe")
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .json(&json!({ "kind": kind, "rollback": rollback }))
        .send()
        .await
        .assert_status(204);
}

async fn dispatch(client: &TestClient) -> usize {
    dispatch_resources(&client.resources().await).await.unwrap()
}

/// Makes every pending delivery due now, as if the backoff had passed.
async fn skip_backoff(url: &str) {
    let mut conn = PgConnection::connect(url).await.unwrap();
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.system', 'on', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE event_deliveries SET next_attempt_at = now()")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn events_reach_interested_subscribers_once() {
    let all = Recorder::new("all", None, 0);
    let orders = Recorder::new("orders", Some("order.placed"), 0);
    let (client, token, _) = app(vec![all.clone(), orders.clone()]).await;

    emit(&client, &token, "product.created", false).await;
    emit(&client, &token, "order.placed", false).await;
    assert_eq!(dispatch(&client).await, 3);
    assert_eq!(dispatch(&client).await, 0);

    assert_eq!(all.kinds(), ["product.created", "order.placed"]);
    assert_eq!(orders.kinds(), ["order.placed"]);
    assert_eq!(all.seen.lock().unwrap()[0].tenant_id, "acme");
}

#[tokio::test]
async fn rolled_back_changes_publish_nothing() {
    let all = Recorder::new("all", None, 0);
    let (client, token, _) = app(vec![all.clone()]).await;
    emit(&client, &token, "product.created", true).await;
    assert_eq!(dispatch(&client).await, 0);
    assert!(all.kinds().is_empty());
}

#[tokio::test]
async fn failures_are_retried_after_backoff() {
    let flaky = Recorder::new("flaky", None, 2);
    let (client, token, url) = app(vec![flaky.clone()]).await;
    emit(&client, &token, "product.created", false).await;

    assert_eq!(dispatch(&client).await, 0);
    // Not due yet: the backoff holds it back.
    assert_eq!(dispatch(&client).await, 0);
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 1);

    skip_backoff(&url).await;
    assert_eq!(dispatch(&client).await, 0);
    skip_backoff(&url).await;
    assert_eq!(dispatch(&client).await, 1);
    assert_eq!(flaky.kinds(), ["product.created"]);
}

#[tokio::test]
async fn the_event_log_is_listed_per_shop() {
    let (client, token, _) = app(vec![]).await;
    emit(&client, &token, "product.created", false).await;
    emit(&client, &token, "order.placed", false).await;
    let page: Value = client
        .get("/api/events?kind=order.placed")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["payload"], json!({ "n": 1 }));
}
