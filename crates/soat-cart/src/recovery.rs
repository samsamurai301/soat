//! Abandoned cart recovery.
//!
//! A shop turns it on (`PUT /api/cart-recovery`) and picks a delay. A
//! scheduled scan ([`scan`]) finds open carts with an email and lines that
//! have sat untouched that long (and less than a week), gives each a link
//! token and publishes `cart.abandoned`, once per cart. The email module
//! reads the token once with [`recovery_mail`] to write the email; the
//! token is in no event. The buyer follows the link to
//! [`recover`], which hands the cart back under a new cart token.

use std::time::Duration;

use serde_json::json;
use soat_core::Permission;
use soat_tenancy::{EventShop, Scope, Shop, Tenant, TenantTx};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::Resources;

use crate::storefront::NewCartResponse;
use crate::{view, CartView};

/// Change the shop's abandoned cart recovery.
pub const CARTS_MANAGE: Permission = Permission::new(
    "carts:manage",
    "Turn abandoned cart emails on or off and set their delay",
)
.for_staff();

/// How often the app scans for abandoned carts.
pub const SCAN_EVERY: Duration = Duration::from_secs(300);
/// Carts untouched for longer than this get no email.
const MAX_AGE_DAYS: i32 = 7;
/// Most carts one shop gets emails for in one scan.
const PER_SCAN: i64 = 200;

/// A shop's abandoned cart recovery.
#[model]
#[derive(sqlx::FromRow)]
pub struct RecoverySettings {
    /// Whether buyers get an email about a cart they left.
    pub enabled: bool,
    /// How long a cart sits untouched before the email, 15 minutes to 7
    /// days.
    pub delay_minutes: i32,
}

impl Default for RecoverySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            delay_minutes: 60,
        }
    }
}

/// Body of a change of the settings.
#[model]
pub struct RecoveryInput {
    pub enabled: bool,
    #[validate(range(min = 15, max = 10080))]
    pub delay_minutes: Option<i32>,
}

/// Body of following a recovery link.
#[model]
pub struct RecoverCart {
    /// The `token` of the link in the email.
    #[validate(length(min = 1, max = 200))]
    pub token: String,
}

async fn settings(conn: &mut PgConnection, scope: &impl Scope) -> Result<RecoverySettings> {
    let row: Option<RecoverySettings> = sqlx::query_as(
        "SELECT enabled, delay_minutes FROM cart_recovery_settings WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(conn)
    .await?;
    Ok(row.unwrap_or_default())
}

/// The shop's abandoned cart recovery (`carts:read`).
#[get("/api/cart-recovery", tag = "carts")]
async fn get_settings(db: Db, tenant: Tenant) -> Result<Json<RecoverySettings>> {
    let mut tx = tenant.begin_for(&db, &crate::CARTS_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Turn abandoned cart emails on or off and set how long a cart sits
/// untouched first (`carts:manage`). Publishes `cart_recovery.updated`.
#[put("/api/cart-recovery", tag = "carts")]
async fn put_settings(
    db: Db,
    tenant: Tenant,
    Valid(Json(s)): Valid<Json<RecoveryInput>>,
) -> Result<Json<RecoverySettings>> {
    let mut tx = tenant.begin_for(&db, &CARTS_MANAGE).await?;
    let saved: RecoverySettings = sqlx::query_as(
        "INSERT INTO cart_recovery_settings (tenant_id, enabled, delay_minutes) \
         VALUES ($1, $2, COALESCE($3, 60)) ON CONFLICT (tenant_id) DO UPDATE SET \
         enabled = EXCLUDED.enabled, \
         delay_minutes = COALESCE($3, cart_recovery_settings.delay_minutes), \
         updated_at = now() RETURNING enabled, delay_minutes",
    )
    .bind(tenant.id())
    .bind(s.enabled)
    .bind(s.delay_minutes)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "cart_recovery.updated", &saved)
        .await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// Follow a recovery link: the cart comes back under a new token (shown
/// once; the old one stops working). The link works until the cart is
/// checked out or a week has passed. Publishes `cart.recovered`.
#[post("/api/storefront/carts/recover", tag = "storefront")]
async fn recover_cart(
    db: Db,
    shop: Shop,
    Valid(Json(r)): Valid<Json<RecoverCart>>,
) -> Result<Json<NewCartResponse>> {
    let (token, cart) = recover(&db, &shop, &r.token).await?;
    Ok(Json(NewCartResponse { token, cart }))
}

pub(crate) fn mount(app: App) -> App {
    app.mount((get_settings, put_settings, recover_cart))
        .schedule(
            zapline::tasks::Schedule::every(SCAN_EVERY).name("soat-cart-recovery"),
            |res: Resources| async move { scan_resources(&res).await.map(|_| ()) },
        )
}

/// The cart a recovery link `token` opens, under a new cart token.
///
/// # Errors
///
/// 404 `invalid_link` for a link that is unknown, of a cart already
/// checked out, or older than a week; database errors.
pub async fn recover(db: &Db, shop: &Shop, token: &str) -> Result<(String, CartView)> {
    let mut tx = shop.begin(db).await?;
    let cart_token = format!("soat_cart_{}", new_token()?);
    let found: Option<(String,)> = sqlx::query_as(&format!(
        "UPDATE carts SET token_hash = $3, updated_at = now() \
         WHERE tenant_id = $1 AND recovery_token_hash = $2 AND status = 'open' \
         AND recovery_sent_at > now() - make_interval(days => {MAX_AGE_DAYS}) RETURNING id"
    ))
    .bind(shop.id())
    .bind(hash_token(token.trim()))
    .bind(hash_token(&cart_token))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id,)) = found else {
        return Err(Error::new(
            StatusCode::NOT_FOUND,
            "this link does not open a cart any more",
        )
        .with_code("invalid_link"));
    };
    let cart = view(&mut tx, shop, &id).await?;
    shop.publish(&mut tx, None, "cart.recovered", &json!({ "id": id }))
        .await?;
    tx.commit().await?;
    Ok((cart_token, cart))
}

/// [`scan`] with the pool from the app's resources.
///
/// # Errors
///
/// Database errors.
pub async fn scan_resources(res: &Resources) -> Result<usize> {
    let pool: PgPool = res.require()?;
    scan(&pool).await
}

/// Marks the carts every shop with recovery on has abandoned and
/// publishes `cart.abandoned` for each; how many.
///
/// # Errors
///
/// Database errors.
pub async fn scan(pool: &PgPool) -> Result<usize> {
    let shops: Vec<(String,)> = sqlx::query_as("SELECT id FROM tenants ORDER BY id")
        .fetch_all(pool)
        .await?;
    let mut marked = 0;
    for (shop,) in shops {
        let scope = EventShop(shop);
        let mut tx = scope.begin_on(pool).await?;
        marked += scan_shop(&mut tx, &scope).await?;
        tx.commit().await?;
    }
    Ok(marked)
}

async fn scan_shop(tx: &mut TenantTx, scope: &EventShop) -> Result<usize> {
    let s = settings(tx, scope).await?;
    if !s.enabled {
        return Ok(0);
    }
    let carts: Vec<(String, String)> = sqlx::query_as(&format!(
        "SELECT c.id, c.email FROM carts c WHERE c.tenant_id = $1 AND c.status = 'open' \
         AND c.email <> '' AND c.recovery_sent_at IS NULL \
         AND c.updated_at < now() - make_interval(mins => $2) \
         AND c.updated_at > now() - make_interval(days => {MAX_AGE_DAYS}) \
         AND EXISTS (SELECT 1 FROM cart_lines l WHERE l.cart_id = c.id) \
         ORDER BY c.updated_at LIMIT {PER_SCAN} FOR UPDATE SKIP LOCKED"
    ))
    .bind(scope.tenant_id())
    .bind(s.delay_minutes)
    .fetch_all(&mut **tx)
    .await?;
    for (id, email) in &carts {
        let token = new_token()?;
        sqlx::query(
            "UPDATE carts SET recovery_sent_at = now(), recovery_token_hash = $3, \
             recovery_token = $4 WHERE tenant_id = $1 AND id = $2",
        )
        .bind(scope.tenant_id())
        .bind(id)
        .bind(hash_token(&token))
        .bind(&token)
        .execute(&mut **tx)
        .await?;
        scope
            .publish(tx, "cart.abandoned", &json!({ "id": id, "email": email }))
            .await?;
    }
    Ok(carts.len())
}

/// An abandoned cart's email: where it goes, the link token, the cart.
#[derive(Debug, Clone)]
pub struct RecoveryMail {
    pub email: String,
    pub token: String,
    pub cart: CartView,
}

/// The email for abandoned cart `id`, once: the token is cleared as it is
/// read. None when it was read before or the cart has been checked out.
///
/// # Errors
///
/// Database errors.
pub async fn recovery_mail(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
) -> Result<Option<RecoveryMail>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "WITH old AS (SELECT id, email, recovery_token FROM carts \
         WHERE tenant_id = $1 AND id = $2 AND status = 'open' \
         AND recovery_token IS NOT NULL FOR UPDATE) \
         UPDATE carts SET recovery_token = NULL FROM old WHERE carts.id = old.id \
         RETURNING old.email, old.recovery_token",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((email, token)) = row else {
        return Ok(None);
    };
    let cart = view(conn, scope, id).await?;
    Ok(Some(RecoveryMail { email, token, cart }))
}
