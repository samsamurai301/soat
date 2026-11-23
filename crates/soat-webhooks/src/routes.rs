//! Managing endpoints and reading the delivery log (`webhooks:manage`).

use soat_tenancy::{Tenant, TenantListing};
use zapline::auth::password::new_token;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::target::{check_target, TargetPolicy};
use crate::{EndpointId, WEBHOOKS_MANAGE};

/// An endpoint to register.
#[model]
pub struct NewEndpoint {
    /// Where to POST, `https://` and public.
    #[validate(length(min = 1, max = 2048))]
    pub url: String,
    /// Event kinds such as `product.created`, or `["*"]` for all.
    #[validate(length(min = 1, max = 100))]
    pub events: Vec<String>,
}

/// A registered endpoint. The secret is only returned at creation.
#[model]
#[derive(sqlx::FromRow)]
pub struct Endpoint {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
}

/// A new endpoint and the secret its deliveries are signed with.
#[model]
pub struct CreatedEndpoint {
    pub endpoint: Endpoint,
    /// Verify `soat-signature` with this. Shown once.
    pub secret: String,
}

/// One delivery of one event to one endpoint.
#[model]
#[derive(sqlx::FromRow)]
pub struct Delivery {
    pub id: String,
    pub event_id: String,
    pub attempts: i32,
    pub delivered: bool,
    pub given_up: bool,
    pub last_status: Option<i32>,
    pub last_error: Option<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        create_endpoint,
        list_endpoints,
        delete_endpoint,
        list_deliveries,
    ))
}

/// Register an endpoint. Only `https://` URLs on public addresses.
#[post("/api/webhooks", tag = "webhooks", status = 201)]
async fn create_endpoint(
    db: Db,
    tenant: Tenant,
    policy: State<TargetPolicy>,
    Valid(Json(body)): Valid<Json<NewEndpoint>>,
) -> Result<(StatusCode, Json<CreatedEndpoint>)> {
    check_target(&body.url, policy.0)
        .map_err(|why| Error::unprocessable(why).with_code("invalid_webhook_url"))?;
    let mut tx = tenant.begin_for(&db, &WEBHOOKS_MANAGE).await?;
    let id = EndpointId::new();
    let secret = format!("whsec_{}", new_token()?);
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, tenant_id, url, events, secret) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(&body.url)
    .bind(&body.events)
    .bind(&secret)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedEndpoint {
            endpoint: Endpoint {
                id: id.to_string(),
                url: body.url,
                events: body.events,
            },
            secret,
        }),
    ))
}

/// The shop's endpoints.
#[get("/api/webhooks", tag = "webhooks")]
async fn list_endpoints(db: Db, tenant: Tenant) -> Result<Json<Vec<Endpoint>>> {
    let mut tx = tenant.begin_for(&db, &WEBHOOKS_MANAGE).await?;
    let endpoints =
        sqlx::query_as::<_, Endpoint>("SELECT id, url, events FROM webhook_endpoints ORDER BY id")
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(Json(endpoints))
}

/// Remove an endpoint and its pending deliveries.
#[delete("/api/webhooks/{id}", tag = "webhooks", status = 204)]
async fn delete_endpoint(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &WEBHOOKS_MANAGE).await?;
    let deleted = sqlx::query("DELETE FROM webhook_endpoints WHERE id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// An endpoint's deliveries, newest first.
#[get("/api/webhooks/{id}/deliveries", tag = "webhooks")]
async fn list_deliveries(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    params: ListParams,
) -> Result<Json<Paged<Delivery>>> {
    let mut tx = tenant.begin_for(&db, &WEBHOOKS_MANAGE).await?;
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM webhook_endpoints WHERE id = $1")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
    if exists.is_none() {
        return Err(Error::not_found());
    }
    let page = TenantListing::new(
        "webhook_deliveries",
        "id, event_id, attempts, delivered_at IS NOT NULL AS delivered, \
         failed_at IS NOT NULL AS given_up, last_status, last_error",
    )
    .scope_text("endpoint_id", id)
    .default_sort("-id")
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}
