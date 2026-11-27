//! Subscriptions: a product sent again every few weeks or months.
//!
//! Staff give a product plans (`/api/subscription-plans`): every N weeks
//! or months, with an optional discount and a shipping price. A signed-in
//! customer subscribes to a plan with an item, a quantity and their
//! address; the first order is placed at once and every renewal after
//! that by a scheduled job ([`renew_due`]), as a completed draft order
//! with the plan's discount, paid with the payment method the customer
//! chose (none leaves the order for them to pay). Customers and staff
//! pause, resume, skip the next order or cancel.
//!
//! A renewal moves the subscription's next date before it places the
//! order, so a crash never places one twice; a renewal that fails is
//! noted (`last_error`, `subscription.renewal_failed`) and tried again the
//! next day.

use serde_json::{json, Value};
use soat_checkout::drafts::{DraftInput, DraftLineInput};
use soat_checkout::TaxProviders;
use soat_core::{id_kind, Permission};
use soat_customers::accounts::SignedIn;
use soat_module::Module;
use soat_orders::PostalAddress;
use soat_payments::{Payment, Providers};
use soat_tenancy::{EventShop, Scope, Shop, Tenant, TenantTx};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::Resources;

id_kind!(PlanId, PlanKind, "spl");
id_kind!(SubscriptionId, SubscriptionKind, "sub");

/// See subscription plans and customers' subscriptions.
pub const SUBSCRIPTIONS_READ: Permission = Permission::new(
    "subscriptions:read",
    "See subscription plans and subscriptions",
)
.for_staff();
/// Change subscription plans and customers' subscriptions.
pub const SUBSCRIPTIONS_MANAGE: Permission = Permission::new(
    "subscriptions:manage",
    "Change subscription plans and customers' subscriptions",
)
.for_staff();

/// How often the app places due renewals.
pub const RENEW_EVERY: std::time::Duration = std::time::Duration::from_secs(300);
/// Most renewals of one shop placed in one run.
const PER_RUN: i64 = 100;

/// The subscriptions module.
pub struct Subscriptions;

impl Module for Subscriptions {
    fn name(&self) -> &'static str {
        "subscriptions"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "catalog",
            "customers",
            "payments",
            "orders",
            "checkout",
        ]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[SUBSCRIPTIONS_READ, SUBSCRIPTIONS_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_plans,
            create_plan,
            update_plan,
            list_subscriptions,
            get_subscription,
            act_on_subscription,
            product_plans,
            my_subscriptions,
            subscribe,
            act_on_mine,
        ))
        .schedule(
            zapline::tasks::Schedule::every(RENEW_EVERY).name("soat-subscriptions"),
            |res: Resources| async move { renew_due(&res).await.map(|_| ()) },
        )
    }
}

/// How a product can be subscribed to.
#[model]
#[derive(sqlx::FromRow)]
pub struct Plan {
    /// e.g. `spl_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub product_id: String,
    /// What buyers see, e.g. `Every month`.
    pub name: String,
    /// `week` or `month`.
    pub interval_unit: String,
    /// 1 to 52: every this many weeks or months.
    pub interval_count: i32,
    /// Off the goods of every order, in hundredths of a percent (1000 is
    /// 10%).
    pub discount_bps: i64,
    /// Shipping charged on every order.
    pub shipping_price_minor: i64,
    /// Inactive plans take no new subscribers; running ones go on.
    pub active: bool,
}

const PLAN_COLUMNS: &str = "id, product_id, name, interval_unit, interval_count, discount_bps, \
     shipping_price_minor, active";

/// The plans of a product, or of the shop.
#[model]
pub struct PlanList {
    pub items: Vec<Plan>,
}

/// Body of a new plan.
#[model]
pub struct NewPlan {
    pub product_id: String,
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub interval_unit: String,
    #[validate(range(min = 1, max = 52))]
    pub interval_count: i32,
    #[validate(range(min = 0, max = 9000))]
    pub discount_bps: Option<i64>,
    #[validate(range(min = 0, max = 100_000_000))]
    pub shipping_price_minor: Option<i64>,
}

/// Body of changing a plan; absent fields stay as they are. The interval
/// of a plan never changes: make a new plan instead.
#[model]
pub struct PlanChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    #[validate(range(min = 0, max = 9000))]
    pub discount_bps: Option<i64>,
    #[validate(range(min = 0, max = 100_000_000))]
    pub shipping_price_minor: Option<i64>,
    pub active: Option<bool>,
}

/// A customer's subscription.
#[model]
#[derive(sqlx::FromRow)]
pub struct Subscription {
    /// e.g. `sub_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub customer_id: String,
    pub plan_id: String,
    pub plan_name: String,
    /// The product, or one of its variants.
    pub item_id: String,
    pub title: String,
    pub quantity: i32,
    pub currency: String,
    /// `active`, `paused` or `canceled`.
    pub status: String,
    /// When the next order is placed, RFC 3339 (while active).
    pub next_order_at: String,
    pub orders_count: i64,
    pub last_order_id: Option<String>,
    /// Why the last renewal failed, if it did.
    pub last_error: Option<String>,
    pub payment_method_id: Option<String>,
    pub created_at: String,
    pub canceled_at: Option<String>,
}

const COLUMNS: &str = "s.id, s.customer_id, s.plan_id, p.name AS plan_name, s.item_id, \
     COALESCE((SELECT pr.title FROM products pr WHERE pr.id = p.product_id), '') AS title, \
     s.quantity, s.currency, s.status, \
     to_char(s.next_order_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS next_order_at, \
     s.orders_count, s.last_order_id, s.last_error, s.payment_method_id, \
     to_char(s.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(s.canceled_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS canceled_at";

const FROM: &str = "subscriptions s JOIN subscription_plans p ON p.id = s.plan_id";

/// Subscriptions, newest first (up to 200).
#[model]
pub struct SubscriptionList {
    pub items: Vec<Subscription>,
}

/// Body of subscribing.
#[model]
pub struct Subscribe {
    pub plan_id: String,
    /// The plan's product, or one of its variants; the product when left
    /// out.
    pub item_id: Option<String>,
    #[validate(range(min = 1, max = 1000))]
    pub quantity: Option<i32>,
    /// Where it goes; the customer's default address when left out.
    pub shipping_address: Option<PostalAddress>,
    /// Defaults to the shipping address.
    pub billing_address: Option<PostalAddress>,
    /// One of the shop's payment methods, for every order; none leaves
    /// each order for the customer to pay.
    pub payment_method_id: Option<String>,
}

/// A new subscription and its first order.
#[model]
pub struct Subscribed {
    pub subscription: Subscription,
    pub order_id: String,
    /// Send as `x-soat-order-token` to see the first order; shown once.
    pub order_token: String,
    pub payment: Option<Payment>,
}

/// Body of a change of state: `pause`, `resume`, `skip` (the next order)
/// or `cancel`.
#[model]
pub struct SubscriptionAction {
    pub action: String,
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_subscription")
}

async fn plan(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Plan> {
    let id = PlanId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {PLAN_COLUMNS} FROM subscription_plans WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's subscription plans (`subscriptions:read`); `?product_id=`
/// for one product's.
#[get("/api/subscription-plans", tag = "subscriptions")]
async fn list_plans(
    db: Db,
    tenant: Tenant,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<PlanList>> {
    let mut tx = tenant.begin_for(&db, &SUBSCRIPTIONS_READ).await?;
    let items = sqlx::query_as(&format!(
        "SELECT {PLAN_COLUMNS} FROM subscription_plans WHERE tenant_id = $1 \
         AND ($2::text IS NULL OR product_id = $2) ORDER BY id LIMIT 500"
    ))
    .bind(tenant.id())
    .bind(q.get("product_id"))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(PlanList { items }))
}

/// Give a product a subscription plan (`subscriptions:manage`). Publishes
/// `subscription_plan.created`.
#[post("/api/subscription-plans", tag = "subscriptions", status = 201)]
async fn create_plan(
    db: Db,
    tenant: Tenant,
    Valid(Json(n)): Valid<Json<NewPlan>>,
) -> Result<(StatusCode, Json<Plan>)> {
    if !["week", "month"].contains(&n.interval_unit.as_str()) {
        return Err(invalid("interval_unit is week or month"));
    }
    let mut tx = tenant.begin_for(&db, &SUBSCRIPTIONS_MANAGE).await?;
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM products WHERE tenant_id = $1 AND id = $2)")
            .bind(tenant.id())
            .bind(&n.product_id)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        return Err(invalid("the shop has no such product"));
    }
    let p: Plan = sqlx::query_as(&format!(
        "INSERT INTO subscription_plans (id, tenant_id, product_id, name, interval_unit, \
         interval_count, discount_bps, shipping_price_minor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {PLAN_COLUMNS}"
    ))
    .bind(PlanId::new().as_str())
    .bind(tenant.id())
    .bind(&n.product_id)
    .bind(n.name.trim())
    .bind(&n.interval_unit)
    .bind(n.interval_count)
    .bind(n.discount_bps.unwrap_or(0))
    .bind(n.shipping_price_minor.unwrap_or(0))
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "subscription_plan.created", &p)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(p)))
}

/// Change a plan, or stop it taking new subscribers (`subscriptions:manage`).
/// Publishes `subscription_plan.updated`.
#[patch("/api/subscription-plans/{id}", tag = "subscriptions")]
async fn update_plan(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<PlanChange>>,
) -> Result<Json<Plan>> {
    let mut tx = tenant.begin_for(&db, &SUBSCRIPTIONS_MANAGE).await?;
    let current = plan(&mut tx, &tenant, &id).await?;
    let p: Plan = sqlx::query_as(&format!(
        "UPDATE subscription_plans SET name = COALESCE($3, name), \
         discount_bps = COALESCE($4, discount_bps), \
         shipping_price_minor = COALESCE($5, shipping_price_minor), \
         active = COALESCE($6, active) WHERE tenant_id = $1 AND id = $2 RETURNING {PLAN_COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(c.name.as_deref().map(str::trim))
    .bind(c.discount_bps)
    .bind(c.shipping_price_minor)
    .bind(c.active)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "subscription_plan.updated", &p)
        .await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// A product's plans that take subscribers, for its page.
#[get("/api/storefront/products/{id}/subscription-plans", tag = "storefront")]
async fn product_plans(db: Db, shop: Shop, Path(id): Path<String>) -> Result<Json<PlanList>> {
    let mut tx = shop.begin(&db).await?;
    let items = plans_for(&mut tx, &shop, &id).await?;
    tx.commit().await?;
    Ok(Json(PlanList { items }))
}

/// Product `product`'s plans that take subscribers.
///
/// # Errors
///
/// Database errors.
pub async fn plans_for(
    conn: &mut PgConnection,
    scope: &impl Scope,
    product: &str,
) -> Result<Vec<Plan>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {PLAN_COLUMNS} FROM subscription_plans WHERE tenant_id = $1 AND product_id = $2 \
         AND active ORDER BY interval_unit DESC, interval_count, id"
    ))
    .bind(scope.tenant_id())
    .bind(product)
    .fetch_all(conn)
    .await?)
}

async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Subscription> {
    let id = SubscriptionId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE s.tenant_id = $1 AND s.id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// Customer `customer`'s subscriptions, newest first.
///
/// # Errors
///
/// Database errors.
pub async fn of_customer(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
) -> Result<Vec<Subscription>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE s.tenant_id = $1 AND s.customer_id = $2 \
         ORDER BY s.id DESC LIMIT 200"
    ))
    .bind(scope.tenant_id())
    .bind(customer)
    .fetch_all(conn)
    .await?)
}

/// The shop's subscriptions, newest first (`subscriptions:read`). Filter
/// with `?status=` or `?customer_id=`.
#[get("/api/subscriptions", tag = "subscriptions")]
async fn list_subscriptions(
    db: Db,
    tenant: Tenant,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<SubscriptionList>> {
    let mut tx = tenant.begin_for(&db, &SUBSCRIPTIONS_READ).await?;
    let items = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE s.tenant_id = $1 \
         AND ($2::text IS NULL OR s.status = $2) AND ($3::text IS NULL OR s.customer_id = $3) \
         ORDER BY s.id DESC LIMIT 200"
    ))
    .bind(tenant.id())
    .bind(q.get("status"))
    .bind(q.get("customer_id"))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(SubscriptionList { items }))
}

/// A subscription (`subscriptions:read`).
#[get("/api/subscriptions/{id}", tag = "subscriptions")]
async fn get_subscription(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Subscription>> {
    let mut tx = tenant.begin_for(&db, &SUBSCRIPTIONS_READ).await?;
    let s = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// The SQL that moves a subscription's next order by one interval of its
/// plan, from `base`.
fn next_after(base: &str) -> String {
    format!(
        "{base} + CASE p.interval_unit WHEN 'week' THEN make_interval(weeks => p.interval_count) \
         ELSE make_interval(months => p.interval_count) END"
    )
}

/// Pauses, resumes, skips the next order of, or cancels subscription `id`.
/// Publishes `subscription.paused`, `.resumed`, `.skipped` or `.canceled`.
///
/// # Errors
///
/// 422 for an unknown action; 409 `subscription_canceled` for a canceled
/// one, `subscription_not_active` to pause or skip one that is not
/// running, `subscription_not_paused` to resume one that is not paused;
/// database errors.
pub async fn change(
    tx: &mut TenantTx,
    scope: &impl Scope,
    id: &str,
    action: &str,
) -> Result<Subscription> {
    let s = load(tx, scope, id).await?;
    if s.status == "canceled" {
        return Err(
            Error::conflict("the subscription is canceled").with_code("subscription_canceled")
        );
    }
    let set = match (action, s.status.as_str()) {
        ("pause", "active") => "status = 'paused'".to_owned(),
        ("skip", "active") => format!("next_order_at = {}", next_after("s.next_order_at")),
        ("resume", "paused") => {
            "status = 'active', next_order_at = GREATEST(s.next_order_at, now())".to_owned()
        }
        ("cancel", _) => "status = 'canceled', canceled_at = now()".to_owned(),
        ("pause" | "skip", _) => {
            return Err(Error::conflict("the subscription is not running")
                .with_code("subscription_not_active"))
        }
        ("resume", _) => {
            return Err(Error::conflict("the subscription is not paused")
                .with_code("subscription_not_paused"))
        }
        (other, _) => {
            return Err(invalid(format!(
                "{other} is not pause, resume, skip or cancel"
            )))
        }
    };
    sqlx::query(&format!(
        "UPDATE subscriptions s SET {set} FROM subscription_plans p \
         WHERE p.id = s.plan_id AND s.tenant_id = $1 AND s.id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(&s.id)
    .execute(&mut **tx)
    .await?;
    let s = load(tx, scope, &s.id).await?;
    let kind = match action {
        "pause" => "subscription.paused",
        "skip" => "subscription.skipped",
        "resume" => "subscription.resumed",
        _ => "subscription.canceled",
    };
    scope.publish(tx, kind, &s).await?;
    Ok(s)
}

/// Pause, resume, skip the next order of, or cancel a subscription
/// (`subscriptions:manage`).
#[post("/api/subscriptions/{id}/actions", tag = "subscriptions")]
async fn act_on_subscription(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(a): Json<SubscriptionAction>,
) -> Result<Json<Subscription>> {
    let mut tx = tenant.begin_for(&db, &SUBSCRIPTIONS_MANAGE).await?;
    let s = change(&mut tx, &tenant, &id, &a.action).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// The signed-in customer's subscriptions, newest first.
#[get("/api/storefront/account/subscriptions", tag = "storefront")]
async fn my_subscriptions(db: Db, signed_in: SignedIn) -> Result<Json<SubscriptionList>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let items = of_customer(&mut tx, &signed_in.shop, &customer).await?;
    tx.commit().await?;
    Ok(Json(SubscriptionList { items }))
}

/// Pause, resume, skip the next order of, or cancel one of the signed-in
/// customer's subscriptions.
#[post(
    "/api/storefront/account/subscriptions/{id}/actions",
    tag = "storefront"
)]
async fn act_on_mine(
    db: Db,
    signed_in: SignedIn,
    Path(id): Path<String>,
    Json(a): Json<SubscriptionAction>,
) -> Result<Json<Subscription>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let s = load(&mut tx, &signed_in.shop, &id).await?;
    if s.customer_id != customer {
        return Err(Error::not_found());
    }
    let s = change(&mut tx, &signed_in.shop, &id, &a.action).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Subscribes customer `customer` and places the first order. Publishes
/// `subscription.created` and the order's events.
///
/// # Errors
///
/// 404 for an unknown or inactive plan; 422 `invalid_subscription` for an
/// item that is not the plan's product or a variant of it, or no address;
/// the first order's errors (e.g. 409 `insufficient_stock`); database
/// errors.
pub async fn start(
    db: &Db,
    shop: &Shop,
    taxes: &TaxProviders,
    providers: &Providers,
    customer: &str,
    s: Subscribe,
) -> Result<Subscribed> {
    let mut tx = shop.begin(db).await?;
    let p = plan(&mut tx, shop, &s.plan_id).await?;
    if !p.active {
        return Err(Error::not_found());
    }
    let item_id = s.item_id.unwrap_or_else(|| p.product_id.clone());
    let item = soat_catalog::stock_item(&mut tx, shop, &item_id)
        .await
        .map_err(|e| invalid(e.message().to_owned()))?
        .filter(|i| i.product_id == p.product_id)
        .ok_or_else(|| invalid("the item is not the plan's product or one of its variants"))?;
    let shipping = match s.shipping_address {
        Some(a) => Some(a),
        None => soat_checkout::drafts::saved_address(&mut tx, shop, customer).await?,
    };
    let Some(shipping) = shipping else {
        return Err(invalid(
            "give a shipping address, or save one in the account first",
        ));
    };
    let billing = s.billing_address.unwrap_or_else(|| shipping.clone());
    let id = SubscriptionId::new();
    sqlx::query(
        "INSERT INTO subscriptions (id, tenant_id, customer_id, plan_id, item_id, quantity, \
         currency, shipping_address, billing_address, payment_method_id, next_order_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())",
    )
    .bind(id.as_str())
    .bind(shop.id())
    .bind(customer)
    .bind(&p.id)
    .bind(&item.id)
    .bind(s.quantity.unwrap_or(1))
    .bind(&item.currency)
    .bind(SqlJson(&shipping))
    .bind(SqlJson(&billing))
    .bind(&s.payment_method_id)
    .execute(&mut *tx)
    .await?;
    let created = load(&mut tx, shop, id.as_str()).await?;
    Scope::publish(shop, &mut tx, "subscription.created", &created).await?;
    let due = advance(&mut tx, shop, id.as_str()).await?;
    let done = soat_checkout::drafts::place_order(
        db,
        tx,
        shop,
        taxes,
        providers,
        &due.input,
        p.discount_bps,
        due.payment_method_id,
    )
    .await?;
    let mut tx = shop.begin(db).await?;
    sqlx::query("UPDATE subscriptions SET last_order_id = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(shop.id())
        .bind(id.as_str())
        .bind(&done.order.id)
        .execute(&mut *tx)
        .await?;
    let subscription = load(&mut tx, shop, id.as_str()).await?;
    tx.commit().await?;
    Ok(Subscribed {
        subscription,
        order_id: done.order.id,
        order_token: done.order_token,
        payment: done.payment,
    })
}

/// Subscribe the signed-in customer to a plan: the first order is placed
/// now, the next ones by themselves. Publishes `subscription.created`
/// and the order's events.
#[post(
    "/api/storefront/account/subscriptions",
    tag = "storefront",
    status = 201
)]
async fn subscribe(
    db: Db,
    signed_in: SignedIn,
    taxes: State<TaxProviders>,
    providers: State<Providers>,
    Valid(Json(s)): Valid<Json<Subscribe>>,
) -> Result<(StatusCode, Json<Subscribed>)> {
    let (tx, customer) = signed_in.begin(&db).await?;
    tx.commit().await?;
    let done = start(&db, &signed_in.shop, &taxes, &providers, &customer, s).await?;
    Ok((StatusCode::CREATED, Json(done)))
}

/// What a due subscription's order is.
struct Due {
    input: DraftInput,
    payment_method_id: Option<String>,
    discount_bps: i64,
}

/// Locks subscription `id`, moves its next order one interval on and
/// counts the order about to be placed; answers that order.
async fn advance(tx: &mut TenantTx, scope: &impl Scope, id: &str) -> Result<Due> {
    type Row = (
        String,
        String,
        i32,
        String,
        Option<SqlJson<Value>>,
        Option<SqlJson<Value>>,
        Option<String>,
        i64,
        i64,
    );
    let row: Row = sqlx::query_as(&format!(
        "UPDATE subscriptions s SET next_order_at = {}, orders_count = s.orders_count + 1, \
         last_error = NULL FROM subscription_plans p WHERE p.id = s.plan_id \
         AND s.tenant_id = $1 AND s.id = $2 RETURNING s.customer_id, s.item_id, s.quantity, \
         s.currency, s.shipping_address, s.billing_address, s.payment_method_id, \
         p.discount_bps, p.shipping_price_minor",
        next_after("GREATEST(s.next_order_at, now())")
    ))
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    let (customer, item, quantity, currency, ship, bill, method, discount_bps, shipping) = row;
    let address = |a: Option<SqlJson<Value>>| a.and_then(|j| serde_json::from_value(j.0).ok());
    Ok(Due {
        input: DraftInput {
            customer_id: Some(customer),
            email: None,
            currency,
            lines: vec![DraftLineInput {
                item_id: item,
                quantity: i64::from(quantity),
                unit_price_minor: None,
            }],
            shipping_address: address(ship),
            billing_address: address(bill),
            vat_number: None,
            shipping_name: Some("Subscription".into()),
            shipping_price_minor: Some(shipping),
            discount_minor: None,
            note: Some(format!("Subscription {id}")),
        },
        payment_method_id: method,
        discount_bps,
    })
}

/// Places the orders of every running subscription that is due; how many.
/// A failed renewal is noted on the subscription, published as
/// `subscription.renewal_failed` and tried again a day later.
///
/// # Errors
///
/// Database errors.
pub async fn renew_due(res: &Resources) -> Result<usize> {
    let pool: PgPool = res.require()?;
    let db = Db(pool.clone());
    let taxes: TaxProviders = res.require()?;
    let providers: Providers = res.require()?;
    let shops: Vec<(String,)> = sqlx::query_as("SELECT id FROM tenants ORDER BY id")
        .fetch_all(&pool)
        .await?;
    let mut placed = 0;
    for (shop,) in shops {
        let scope = EventShop(shop);
        let mut tx = scope.begin_on(&pool).await?;
        let due: Vec<(String,)> = sqlx::query_as(&format!(
            "SELECT id FROM subscriptions WHERE tenant_id = $1 AND status = 'active' \
             AND next_order_at <= now() ORDER BY next_order_at LIMIT {PER_RUN}"
        ))
        .bind(scope.tenant_id())
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        for (id,) in due {
            // Move the date first, in its own transaction: a crash after it
            // misses one order rather than placing it twice.
            let mut tx = scope.begin_on(&pool).await?;
            let locked: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM subscriptions WHERE tenant_id = $1 AND id = $2 \
                 AND status = 'active' AND next_order_at <= now() FOR UPDATE SKIP LOCKED",
            )
            .bind(scope.tenant_id())
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
            if locked.is_none() {
                continue;
            }
            let order = advance(&mut tx, &scope, &id).await?;
            tx.commit().await?;
            let tx = scope.begin_on(&pool).await?;
            let outcome = soat_checkout::drafts::place_order(
                &db,
                tx,
                &scope,
                &taxes,
                &providers,
                &order.input,
                order.discount_bps,
                order.payment_method_id.clone(),
            )
            .await;
            let mut tx = scope.begin_on(&pool).await?;
            match outcome {
                Ok(done) => {
                    placed += 1;
                    sqlx::query(
                        "UPDATE subscriptions SET last_order_id = $3 \
                         WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(scope.tenant_id())
                    .bind(&id)
                    .bind(&done.order.id)
                    .execute(&mut *tx)
                    .await?;
                    let s = load(&mut tx, &scope, &id).await?;
                    scope
                        .publish(
                            &mut tx,
                            "subscription.renewed",
                            &json!({ "id": s.id, "order_id": done.order.id,
                                     "orders_count": s.orders_count }),
                        )
                        .await?;
                }
                Err(e) => {
                    sqlx::query(
                        "UPDATE subscriptions SET last_error = $3, orders_count = orders_count - 1, \
                         next_order_at = now() + interval '1 day' WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(scope.tenant_id())
                    .bind(&id)
                    .bind(e.message())
                    .execute(&mut *tx)
                    .await?;
                    scope
                        .publish(
                            &mut tx,
                            "subscription.renewal_failed",
                            &json!({ "id": id, "error": e.message() }),
                        )
                        .await?;
                }
            }
            tx.commit().await?;
        }
    }
    Ok(placed)
}
