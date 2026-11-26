//! Loyalty points and referrals.
//!
//! A shop turns its program on (`PUT /api/loyalty`). Customers then earn
//! points when an order of theirs is paid (`points_per_unit` for each
//! whole unit of its goods, after discounts) and lose them again when it
//! is refunded in full. They turn points into store credit
//! (`points_per_credit` points for one unit of the currency), which they
//! spend at checkout like any store credit.
//!
//! Every customer has a referral code. A new customer who enters a
//! friend's code before their first paid order gets `referee_points`
//! when that order is paid, and the friend gets `referrer_points`.
//!
//! Every change of a balance is an entry of the customer's ledger and
//! publishes `loyalty.points_changed`.

use std::sync::Arc;

use serde_json::json;
use soat_core::{id_kind, Currency, Permission};
use soat_customers::accounts::SignedIn;
use soat_giftcards::GiftCard;
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_tenancy::{EventShop, Scope, Tenant, TenantTx};
use zapline::auth::password::new_token;
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::Resources;

id_kind!(LoyaltyEntryId, LoyaltyEntryKind, "lpt");

/// See the loyalty program and customers' points.
pub const LOYALTY_READ: Permission = Permission::new(
    "loyalty:read",
    "See the loyalty program and customers' points",
)
.for_staff();
/// Change the loyalty program and adjust customers' points.
pub const LOYALTY_MANAGE: Permission = Permission::new(
    "loyalty:manage",
    "Change the loyalty program and adjust customers' points",
)
.for_staff();

/// Most points one change moves.
const MAX_POINTS: i64 = 1_000_000_000;

/// The loyalty module.
pub struct Loyalty;

impl Module for Loyalty {
    fn name(&self) -> &'static str {
        "loyalty"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "customers", "orders", "giftcards"]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[LOYALTY_READ, LOYALTY_MANAGE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(EarnPoints)]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            get_program,
            put_program,
            customer_points,
            adjust_points,
            my_points,
            redeem_points,
            enter_referral,
        ))
    }
}

/// A shop's loyalty program.
#[model]
#[derive(sqlx::FromRow)]
pub struct Program {
    /// Whether customers earn and spend points.
    pub enabled: bool,
    /// Points for each whole unit of the currency an order's goods cost,
    /// after discounts (e.g. 1 point per euro).
    pub points_per_unit: i64,
    /// Points for one whole unit of store credit (e.g. 100 points make
    /// 1 euro).
    pub points_per_credit: i64,
    /// Fewest points turned into store credit at once.
    pub min_redeem: i64,
    /// Points for a customer whose friend's first order is paid.
    pub referrer_points: i64,
    /// Points for the friend, with that first order.
    pub referee_points: i64,
}

impl Default for Program {
    fn default() -> Self {
        Self {
            enabled: false,
            points_per_unit: 1,
            points_per_credit: 100,
            min_redeem: 100,
            referrer_points: 500,
            referee_points: 250,
        }
    }
}

/// Body of changing the program; absent numbers stay as they are.
#[model]
pub struct ProgramInput {
    pub enabled: bool,
    #[validate(range(min = 0, max = 1000))]
    pub points_per_unit: Option<i64>,
    #[validate(range(min = 1, max = 1_000_000))]
    pub points_per_credit: Option<i64>,
    #[validate(range(min = 1, max = 1_000_000_000))]
    pub min_redeem: Option<i64>,
    #[validate(range(min = 0, max = 1_000_000))]
    pub referrer_points: Option<i64>,
    #[validate(range(min = 0, max = 1_000_000))]
    pub referee_points: Option<i64>,
}

/// A change of a customer's points.
#[model]
#[derive(sqlx::FromRow)]
pub struct Entry {
    pub id: String,
    /// `earned`, `reversed`, `redeemed`, `referral`, `welcome` or
    /// `adjusted`.
    pub kind: String,
    /// Positive adds, negative takes away.
    pub points: i64,
    /// The balance after it.
    pub balance: i64,
    pub order_id: Option<String>,
    pub note: String,
    pub created_at: String,
}

/// A customer's points and their history, newest first (up to 100).
#[model]
pub struct Points {
    pub customer_id: String,
    pub balance: i64,
    /// The code friends enter to be referred by this customer.
    pub referral_code: String,
    /// The customer who referred this one, if any.
    pub referred_by: Option<String>,
    pub entries: Vec<Entry>,
}

/// The signed-in customer's points, with the program's terms.
#[model]
pub struct MyPoints {
    pub program: Program,
    pub balance: i64,
    pub referral_code: String,
    /// Whether the customer entered a friend's code.
    pub referred: bool,
    /// Whether they still can: before their first paid order, once.
    pub can_enter_referral: bool,
    pub entries: Vec<Entry>,
}

/// Body of adjusting a customer's points.
#[model]
pub struct Adjustment {
    /// Positive adds, negative takes away; never below zero.
    #[validate(range(min = -1_000_000_000, max = 1_000_000_000))]
    pub points: i64,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

/// Body of turning points into store credit.
#[model]
pub struct Redeem {
    #[validate(range(min = 1, max = 1_000_000_000))]
    pub points: i64,
    /// The store credit's currency, e.g. `EUR`.
    pub currency: String,
}

/// Points turned into store credit.
#[model]
pub struct Redeemed {
    /// The points left.
    pub balance: i64,
    /// The customer's store credit after it.
    pub store_credit: GiftCard,
}

/// Body of entering a friend's referral code.
#[model]
pub struct ReferralInput {
    #[validate(length(min = 1, max = 64))]
    pub code: String,
}

fn invalid(code: &'static str, m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code(code)
}

/// The shop's program, or the default (off) when it never set one.
///
/// # Errors
///
/// Database errors.
pub async fn program(conn: &mut PgConnection, scope: &impl Scope) -> Result<Program> {
    let row: Option<Program> = sqlx::query_as(
        "SELECT enabled, points_per_unit, points_per_credit, min_redeem, referrer_points, \
         referee_points FROM loyalty_settings WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(conn)
    .await?;
    Ok(row.unwrap_or_default())
}

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn new_code() -> Result<String> {
    Ok(new_token()?
        .bytes()
        .take(8)
        .map(|b| char::from(ALPHABET[usize::from(b) % ALPHABET.len()]))
        .collect())
}

/// A code as people type it: uppercase letters and digits only.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Opens the customer's account when they have none, then locks it.
/// Answers its balance, referral code, referrer and whether the referral
/// was rewarded.
async fn lock_account(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
) -> Result<(i64, String, Option<String>, bool)> {
    for _ in 0..5 {
        let made = sqlx::query(
            "INSERT INTO loyalty_accounts (tenant_id, customer_id, referral_code) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(scope.tenant_id())
        .bind(customer)
        .bind(new_code()?)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        let row: Option<(i64, String, Option<String>, bool)> = sqlx::query_as(
            "SELECT balance, referral_code, referred_by, referral_rewarded FROM loyalty_accounts \
             WHERE tenant_id = $1 AND customer_id = $2 FOR UPDATE",
        )
        .bind(scope.tenant_id())
        .bind(customer)
        .fetch_optional(&mut *conn)
        .await?;
        match row {
            Some(r) => return Ok(r),
            // The code was taken by someone else: try another.
            None if made == 0 => continue,
            None => break,
        }
    }
    Err(Error::internal("could not open a loyalty account"))
}

async fn entries(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
) -> Result<Vec<Entry>> {
    Ok(sqlx::query_as(
        "SELECT id, kind, points, balance, order_id, note, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at \
         FROM loyalty_entries WHERE tenant_id = $1 AND customer_id = $2 ORDER BY id DESC LIMIT 100",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .fetch_all(conn)
    .await?)
}

/// Customer `customer`'s points and history.
///
/// # Errors
///
/// Database errors.
pub async fn points(tx: &mut TenantTx, scope: &impl Scope, customer: &str) -> Result<Points> {
    let (balance, referral_code, referred_by, _) = lock_account(tx, scope, customer).await?;
    Ok(Points {
        customer_id: customer.to_owned(),
        balance,
        referral_code,
        referred_by,
        entries: entries(tx, scope, customer).await?,
    })
}

/// Moves customer `customer`'s points by `points` and records why. With
/// an `order_id`, an order moves them once per `kind`: a second time
/// answers None. Below zero is a 422 `insufficient_points`. Publishes
/// `loyalty.points_changed`.
async fn move_points(
    tx: &mut TenantTx,
    scope: &impl Scope,
    customer: &str,
    kind: &str,
    points: i64,
    order_id: Option<&str>,
    note: &str,
) -> Result<Option<Entry>> {
    let (balance, ..) = lock_account(tx, scope, customer).await?;
    if let Some(order) = order_id {
        let (done,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM loyalty_entries WHERE tenant_id = $1 \
             AND customer_id = $2 AND order_id = $3 AND kind = $4)",
        )
        .bind(scope.tenant_id())
        .bind(customer)
        .bind(order)
        .bind(kind)
        .fetch_one(&mut **tx)
        .await?;
        if done {
            return Ok(None);
        }
    }
    let after = balance.saturating_add(points);
    if after < 0 {
        return Err(invalid(
            "insufficient_points",
            format!("the customer has {balance} points"),
        ));
    }
    sqlx::query(
        "UPDATE loyalty_accounts SET balance = $3 WHERE tenant_id = $1 AND customer_id = $2",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .bind(after)
    .execute(&mut **tx)
    .await?;
    let entry: Entry = sqlx::query_as(
        "INSERT INTO loyalty_entries (id, tenant_id, customer_id, kind, points, balance, \
         order_id, note) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id, kind, points, \
         balance, order_id, note, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at",
    )
    .bind(LoyaltyEntryId::new().as_str())
    .bind(scope.tenant_id())
    .bind(customer)
    .bind(kind)
    .bind(points)
    .bind(after)
    .bind(order_id)
    .bind(note.trim())
    .fetch_one(&mut **tx)
    .await?;
    scope
        .publish(
            tx,
            "loyalty.points_changed",
            &json!({ "customer_id": customer, "kind": kind, "points": points,
                     "balance": after, "order_id": order_id }),
        )
        .await?;
    Ok(Some(entry))
}

fn off() -> Error {
    Error::conflict("the shop has no loyalty program").with_code("loyalty_off")
}

/// Turns `points` of customer `customer` into store credit in `currency`.
/// Publishes `loyalty.points_changed` and the store credit's events.
///
/// # Errors
///
/// 409 `loyalty_off` when the program is off; 422 `invalid_redeem` below
/// the program's minimum or for an unknown currency; 422
/// `insufficient_points`; database errors.
pub async fn redeem(
    tx: &mut TenantTx,
    scope: &impl Scope,
    customer: &str,
    points: i64,
    currency: &str,
) -> Result<Redeemed> {
    let p = program(tx, scope).await?;
    if !p.enabled {
        return Err(off());
    }
    let currency =
        Currency::parse(currency.trim()).map_err(|e| invalid("invalid_redeem", e.to_string()))?;
    if points < p.min_redeem {
        return Err(invalid(
            "invalid_redeem",
            format!("turn at least {} points into store credit", p.min_redeem),
        ));
    }
    let unit = 10_i128.pow(currency.decimals());
    let value = i128::from(points) * unit / i128::from(p.points_per_credit.max(1));
    let value = i64::try_from(value).unwrap_or(i64::MAX);
    if value <= 0 {
        return Err(invalid(
            "invalid_redeem",
            "these points are worth nothing yet",
        ));
    }
    let entry = move_points(
        tx,
        scope,
        customer,
        "redeemed",
        -points,
        None,
        &format!("Store credit {}", currency.format(value)),
    )
    .await?
    .ok_or_else(|| Error::internal("points not taken"))?;
    let store_credit = soat_giftcards::add_store_credit(
        tx,
        scope,
        customer,
        currency.as_str(),
        value,
        "Loyalty points",
    )
    .await?;
    Ok(Redeemed {
        balance: entry.balance,
        store_credit,
    })
}

/// Records that customer `customer` was referred by the owner of `code`.
/// Both get their points when the customer's first order is paid.
/// Publishes `loyalty.referral_entered`.
///
/// # Errors
///
/// 409 `loyalty_off`; 422 `invalid_referral` for an unknown code, the
/// customer's own, a second code, or a customer who has had a paid order;
/// database errors.
pub async fn enter_code(
    tx: &mut TenantTx,
    scope: &impl Scope,
    customer: &str,
    code: &str,
) -> Result<()> {
    if !program(tx, scope).await?.enabled {
        return Err(off());
    }
    let code = normalize(code);
    let owner: Option<(String,)> = sqlx::query_as(
        "SELECT customer_id FROM loyalty_accounts WHERE tenant_id = $1 AND referral_code = $2",
    )
    .bind(scope.tenant_id())
    .bind(&code)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((owner,)) = owner else {
        return Err(invalid("invalid_referral", "no such referral code"));
    };
    if owner == customer {
        return Err(invalid("invalid_referral", "this is your own code"));
    }
    let (_, _, referred_by, _) = lock_account(tx, scope, customer).await?;
    if referred_by.is_some() {
        return Err(invalid("invalid_referral", "you entered a code before"));
    }
    if has_paid_order(tx, scope, customer).await? {
        return Err(invalid(
            "invalid_referral",
            "a code is entered before the first order",
        ));
    }
    sqlx::query(
        "UPDATE loyalty_accounts SET referred_by = $3 WHERE tenant_id = $1 AND customer_id = $2",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .bind(&owner)
    .execute(&mut **tx)
    .await?;
    scope
        .publish(
            tx,
            "loyalty.referral_entered",
            &json!({ "customer_id": customer, "referrer_id": owner }),
        )
        .await?;
    Ok(())
}

async fn has_paid_order(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
) -> Result<bool> {
    let (yes,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM orders WHERE tenant_id = $1 AND customer_id = $2 \
         AND financial_status IN ('paid', 'partially_refunded', 'refunded'))",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .fetch_one(conn)
    .await?;
    Ok(yes)
}

/// The signed-in customer's points and the program's terms.
///
/// # Errors
///
/// Database errors.
pub async fn mine(tx: &mut TenantTx, scope: &impl Scope, customer: &str) -> Result<MyPoints> {
    let program = program(tx, scope).await?;
    let p = points(tx, scope, customer).await?;
    let can_enter_referral =
        program.enabled && p.referred_by.is_none() && !has_paid_order(tx, scope, customer).await?;
    Ok(MyPoints {
        program,
        balance: p.balance,
        referral_code: p.referral_code,
        referred: p.referred_by.is_some(),
        can_enter_referral,
        entries: p.entries,
    })
}

/// The shop's loyalty program (`loyalty:read`).
#[get("/api/loyalty", tag = "loyalty")]
async fn get_program(db: Db, tenant: Tenant) -> Result<Json<Program>> {
    let mut tx = tenant.begin_for(&db, &LOYALTY_READ).await?;
    let p = program(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Turn the loyalty program on or off and set its terms
/// (`loyalty:manage`). Publishes `loyalty.updated`.
#[put("/api/loyalty", tag = "loyalty")]
async fn put_program(
    db: Db,
    tenant: Tenant,
    Valid(Json(p)): Valid<Json<ProgramInput>>,
) -> Result<Json<Program>> {
    let mut tx = tenant.begin_for(&db, &LOYALTY_MANAGE).await?;
    let d = Program::default();
    let saved: Program = sqlx::query_as(
        "INSERT INTO loyalty_settings (tenant_id, enabled, points_per_unit, points_per_credit, \
         min_redeem, referrer_points, referee_points) \
         VALUES ($1, $2, COALESCE($3, $8), COALESCE($4, $9), COALESCE($5, $10), \
         COALESCE($6, $11), COALESCE($7, $12)) \
         ON CONFLICT (tenant_id) DO UPDATE SET enabled = EXCLUDED.enabled, \
         points_per_unit = COALESCE($3, loyalty_settings.points_per_unit), \
         points_per_credit = COALESCE($4, loyalty_settings.points_per_credit), \
         min_redeem = COALESCE($5, loyalty_settings.min_redeem), \
         referrer_points = COALESCE($6, loyalty_settings.referrer_points), \
         referee_points = COALESCE($7, loyalty_settings.referee_points), updated_at = now() \
         RETURNING enabled, points_per_unit, points_per_credit, min_redeem, referrer_points, \
         referee_points",
    )
    .bind(tenant.id())
    .bind(p.enabled)
    .bind(p.points_per_unit)
    .bind(p.points_per_credit)
    .bind(p.min_redeem)
    .bind(p.referrer_points)
    .bind(p.referee_points)
    .bind(d.points_per_unit)
    .bind(d.points_per_credit)
    .bind(d.min_redeem)
    .bind(d.referrer_points)
    .bind(d.referee_points)
    .fetch_one(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "loyalty.updated", &saved).await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// A customer's points and their history (`loyalty:read`).
#[get("/api/loyalty/customers/{customer_id}", tag = "loyalty")]
async fn customer_points(
    db: Db,
    tenant: Tenant,
    Path(customer): Path<String>,
) -> Result<Json<Points>> {
    let mut tx = tenant.begin_for(&db, &LOYALTY_READ).await?;
    soat_customers::load(&mut tx, &tenant, &customer).await?;
    let p = points(&mut tx, &tenant, &customer).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Add points to a customer or take them away (`loyalty:manage`), e.g.
/// as a thank you. Below zero is a 422 `insufficient_points`. Publishes
/// `loyalty.points_changed`.
#[post("/api/loyalty/customers/{customer_id}/adjustments", tag = "loyalty")]
async fn adjust_points(
    db: Db,
    tenant: Tenant,
    Path(customer): Path<String>,
    Valid(Json(a)): Valid<Json<Adjustment>>,
) -> Result<Json<Points>> {
    if a.points == 0 {
        return Err(invalid("validation_error", "an adjustment is not zero"));
    }
    let mut tx = tenant.begin_for(&db, &LOYALTY_MANAGE).await?;
    soat_customers::load(&mut tx, &tenant, &customer).await?;
    let note = a.note.as_deref().unwrap_or("");
    move_points(
        &mut tx, &tenant, &customer, "adjusted", a.points, None, note,
    )
    .await?;
    let p = points(&mut tx, &tenant, &customer).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// The signed-in customer's points, their history, their referral code
/// and the program's terms.
#[get("/api/storefront/account/loyalty", tag = "storefront")]
async fn my_points(db: Db, signed_in: SignedIn) -> Result<Json<MyPoints>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let m = mine(&mut tx, &signed_in.shop, &customer).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Turn the signed-in customer's points into store credit, spent at
/// checkout. Publishes `loyalty.points_changed`.
#[post("/api/storefront/account/loyalty/redeem", tag = "storefront")]
async fn redeem_points(
    db: Db,
    signed_in: SignedIn,
    Valid(Json(r)): Valid<Json<Redeem>>,
) -> Result<Json<Redeemed>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let done = redeem(&mut tx, &signed_in.shop, &customer, r.points, &r.currency).await?;
    tx.commit().await?;
    Ok(Json(done))
}

/// Enter the code of the friend who referred the signed-in customer,
/// before their first paid order. Both get points when it is paid.
/// Publishes `loyalty.referral_entered`.
#[post(
    "/api/storefront/account/loyalty/referral",
    tag = "storefront",
    status = 204
)]
async fn enter_referral(
    db: Db,
    signed_in: SignedIn,
    Valid(Json(r)): Valid<Json<ReferralInput>>,
) -> Result<StatusCode> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    enter_code(&mut tx, &signed_in.shop, &customer, &r.code).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Points for the goods of an order that cost `minor` in `currency`.
fn earned(program: &Program, minor: i64, currency: &str) -> i64 {
    let Ok(c) = Currency::parse(currency) else {
        return 0;
    };
    let unit = 10_i128.pow(c.decimals());
    let points = i128::from(minor.max(0)) * i128::from(program.points_per_unit) / unit;
    i64::try_from(points).unwrap_or(MAX_POINTS).min(MAX_POINTS)
}

/// Gives points for paid orders (and referrals) and takes them back when
/// an order is refunded in full.
struct EarnPoints;

impl Subscriber for EarnPoints {
    fn name(&self) -> &'static str {
        "loyalty-points"
    }

    fn wants(&self, kind: &str) -> bool {
        kind == "order.paid" || kind == "order.updated"
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let order = &event.payload;
            let (Some(order_id), Some(customer)) =
                (order["id"].as_str(), order["customer_id"].as_str())
            else {
                return Ok(());
            };
            let refunded = order["financial_status"] == "refunded";
            if event.kind == "order.updated" && !refunded {
                return Ok(());
            }
            let pool: PgPool = res.require()?;
            let scope = EventShop(event.tenant_id.clone());
            let mut tx = scope.begin_on(&pool).await?;
            if event.kind == "order.updated" {
                take_back(&mut tx, &scope, customer, order_id).await?;
                tx.commit().await?;
                return Ok(());
            }
            let p = program(&mut tx, &scope).await?;
            if !p.enabled {
                return Ok(());
            }
            let goods = order["subtotal_minor"].as_i64().unwrap_or(0)
                - order["discount_minor"].as_i64().unwrap_or(0);
            let points = earned(&p, goods, order["currency"].as_str().unwrap_or(""));
            if points > 0 {
                let note = format!("Order {}", order["number"].as_i64().unwrap_or(0));
                move_points(
                    &mut tx,
                    &scope,
                    customer,
                    "earned",
                    points,
                    Some(order_id),
                    &note,
                )
                .await?;
            }
            let (_, _, referrer, rewarded) = lock_account(&mut tx, &scope, customer).await?;
            if let (Some(referrer), false) = (referrer, rewarded) {
                sqlx::query(
                    "UPDATE loyalty_accounts SET referral_rewarded = TRUE \
                     WHERE tenant_id = $1 AND customer_id = $2",
                )
                .bind(&event.tenant_id)
                .bind(customer)
                .execute(&mut *tx)
                .await?;
                if p.referrer_points > 0 {
                    move_points(
                        &mut tx,
                        &scope,
                        &referrer,
                        "referral",
                        p.referrer_points,
                        Some(order_id),
                        "A friend's first order",
                    )
                    .await?;
                }
                if p.referee_points > 0 {
                    move_points(
                        &mut tx,
                        &scope,
                        customer,
                        "welcome",
                        p.referee_points,
                        Some(order_id),
                        "Referred by a friend",
                    )
                    .await?;
                }
            }
            tx.commit().await?;
            Ok(())
        })
    }
}

/// Takes back the points order `order_id` earned, as far as the customer
/// still has them.
async fn take_back(
    tx: &mut TenantTx,
    scope: &EventShop,
    customer: &str,
    order: &str,
) -> Result<()> {
    let got: Option<(i64,)> = sqlx::query_as(
        "SELECT points FROM loyalty_entries WHERE tenant_id = $1 AND customer_id = $2 \
         AND order_id = $3 AND kind = 'earned'",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .bind(order)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((points,)) = got else {
        return Ok(());
    };
    let (balance, ..) = lock_account(tx, scope, customer).await?;
    let back = points.min(balance);
    if back > 0 {
        move_points(
            tx,
            scope,
            customer,
            "reversed",
            -back,
            Some(order),
            "Order refunded",
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_follow_whole_units() {
        let p = Program::default();
        assert_eq!(earned(&p, 1999, "EUR"), 19);
        assert_eq!(earned(&p, 1999, "JPY"), 1999);
        assert_eq!(earned(&p, -5, "EUR"), 0);
        assert_eq!(earned(&p, 100, "nope"), 0);
        let p = Program {
            points_per_unit: 3,
            ..Program::default()
        };
        assert_eq!(earned(&p, 1000, "EUR"), 30);
    }

    #[test]
    fn codes_are_typed_loosely() {
        assert_eq!(normalize(" ab-cd 12 "), "ABCD12");
        let c = new_code().unwrap();
        assert_eq!(c.len(), 8);
        assert_eq!(normalize(&c), c);
    }
}
