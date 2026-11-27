//! Promotions: discount codes and automatic discounts.
//!
//! A discount takes a percentage or a fixed amount off the goods, makes
//! shipping free, or discounts the cheapest units of every lot (buy X get
//! Y), between optional dates, above an optional minimum, up to an
//! optional number of uses (and once per customer if asked), on every
//! product or a collection's. A code is entered at checkout; an automatic
//! discount needs none, and the one worth most applies by itself, before
//! any code. Checkout asks [`automatic`] and [`check`] what they are worth
//! for a cart and records their use with [`redeem`] when the order is
//! placed.

use soat_core::discount::{allocate, amounts, buy_x_get_y, normalize_code, DiscountKind};
use soat_core::{id_kind, Currency, Permission};
use soat_module::Module;
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(DiscountId, DiscountKind_, "dsc");

/// See discounts.
pub const PROMOTIONS_READ: Permission =
    Permission::new("promotions:read", "See discount codes and their use").for_staff();
/// Create, change and delete discounts.
pub const PROMOTIONS_MANAGE: Permission = Permission::new(
    "promotions:manage",
    "Create, change and delete discount codes",
)
.for_staff();

/// The promotions module.
pub struct Promotions;

impl Module for Promotions {
    fn name(&self) -> &'static str {
        "promotions"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "catalog"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[PROMOTIONS_READ, PROMOTIONS_MANAGE]
    }

    fn personal_data(&self) -> &'static [soat_module::PersonalData] {
        &[soat_module::PersonalData {
            table: "discount_redemptions",
            matches: "customer_id = $2 OR lower(email) = $3",
            export: &["discount_id", "order_id", "amount_minor", "created_at"],
            erase: soat_module::Erase::Anonymize(&[("customer_id", "NULL"), ("email", "''")]),
        }]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_discounts,
            create_discount,
            get_discount,
            update_discount,
            delete_discount,
        ))
    }
}

/// A discount: a code people enter at checkout, or an automatic
/// discount that applies by itself when its conditions are met.
#[model]
#[derive(sqlx::FromRow)]
pub struct Discount {
    /// e.g. `dsc_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Unique in the shop, uppercase, e.g. `SUMMER10`; null for an
    /// automatic discount.
    pub code: Option<String>,
    /// What the buyer sees, e.g. `Summer sale`; the code by default.
    pub title: String,
    /// Applies at checkout without a code.
    pub automatic: bool,
    /// `percentage`, `fixed_amount`, `free_shipping` or `buy_x_get_y`.
    pub kind: String,
    /// Basis points for a percentage and for buy X get Y (what comes off
    /// the units got; 10000 = free), minor units for a fixed amount.
    pub value: i64,
    /// For buy X get Y: every `buy_quantity + get_quantity` units, the
    /// cheapest `get_quantity` are discounted.
    pub buy_quantity: Option<i64>,
    pub get_quantity: Option<i64>,
    /// Only the products of this collection count and are discounted;
    /// null for every product.
    pub collection_id: Option<String>,
    /// The currency of a fixed amount and of the minimum; null for any.
    pub currency: Option<String>,
    /// The least the lines that count must come to.
    pub min_subtotal_minor: Option<i64>,
    /// RFC 3339; null for no bound.
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    /// Most uses overall; null for no limit.
    pub usage_limit: Option<i64>,
    pub once_per_customer: bool,
    pub used_count: i64,
    pub active: bool,
}

const COLUMNS: &str = "id, code, title, automatic, kind, value, buy_quantity, get_quantity, \
     collection_id, currency, min_subtotal_minor, \
     to_char(starts_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS starts_at, \
     to_char(ends_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS ends_at, \
     usage_limit, once_per_customer, used_count, active";

/// Body of creating or replacing a discount.
#[model]
pub struct DiscountInput {
    /// Required unless `automatic`; must be left out when it is.
    pub code: Option<String>,
    /// Required for an automatic discount.
    #[validate(length(max = 200))]
    pub title: Option<String>,
    #[serde(default)]
    pub automatic: bool,
    pub kind: String,
    #[serde(default)]
    pub value: i64,
    #[validate(range(min = 1, max = 1000))]
    pub buy_quantity: Option<i64>,
    #[validate(range(min = 1, max = 1000))]
    pub get_quantity: Option<i64>,
    pub collection_id: Option<String>,
    pub currency: Option<String>,
    #[validate(range(min = 0))]
    pub min_subtotal_minor: Option<i64>,
    /// RFC 3339, e.g. `2026-12-01T00:00:00Z`.
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    #[validate(range(min = 1))]
    pub usage_limit: Option<i64>,
    #[serde(default)]
    pub once_per_customer: bool,
    pub active: Option<bool>,
}

fn kind_of(kind: &str, value: i64) -> Option<DiscountKind> {
    match kind {
        "percentage" => Some(DiscountKind::Percentage(value)),
        "fixed_amount" => Some(DiscountKind::FixedAmount(value)),
        "free_shipping" => Some(DiscountKind::FreeShipping),
        _ => None,
    }
}

/// A discount's input made ready to store.
struct Checked {
    code: Option<String>,
    title: String,
    currency: Option<String>,
    buy: Option<i64>,
    get: Option<i64>,
}

fn checked(d: &DiscountInput) -> Result<Checked> {
    let bad = |m: String| Error::unprocessable(m).with_code("invalid_discount");
    let raw = d.code.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let code = match (d.automatic, raw) {
        (true, Some(_)) => return Err(bad("an automatic discount has no code".into())),
        (true, None) => None,
        (false, None) => return Err(bad("a discount needs a code, or automatic".into())),
        (false, Some(c)) => Some(
            normalize_code(c)
                .ok_or_else(|| bad("a code is 1 to 50 of A-Z, 0-9, - and _".into()))?,
        ),
    };
    let title = d
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .or_else(|| code.clone())
        .ok_or_else(|| bad("an automatic discount needs a title".into()))?;
    match d.kind.as_str() {
        "percentage" | "buy_x_get_y" if !(1..=10_000).contains(&d.value) => {
            return Err(bad("a percentage is 1 to 10000 basis points".into()))
        }
        "fixed_amount" if d.value < 1 => return Err(bad("a fixed amount is at least 1".into())),
        "fixed_amount" if d.currency.is_none() => {
            return Err(bad("a fixed amount needs a currency".into()))
        }
        "buy_x_get_y" if d.buy_quantity.is_none() || d.get_quantity.is_none() => {
            return Err(bad("buy X get Y needs buy_quantity and get_quantity".into()))
        }
        "percentage" | "fixed_amount" | "free_shipping" | "buy_x_get_y" => {}
        k => {
            return Err(bad(format!(
                "{k} is not percentage, fixed_amount, free_shipping or buy_x_get_y"
            )))
        }
    }
    let bxgy = d.kind == "buy_x_get_y";
    let currency = d
        .currency
        .as_deref()
        .map(|c| Currency::parse(c).map(|c| c.as_str().to_owned()))
        .transpose()
        .map_err(|e| bad(e.to_string()))?;
    Ok(Checked {
        code,
        title,
        currency,
        buy: d.buy_quantity.filter(|_| bxgy),
        get: d.get_quantity.filter(|_| bxgy),
    })
}

async fn write(
    tx: &mut TenantTx,
    tenant: &Tenant,
    id: &str,
    d: &DiscountInput,
    insert: bool,
) -> Result<Option<Discount>> {
    let c = checked(d)?;
    let collection = d
        .collection_id
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    if let Some(col) = collection {
        if soat_catalog::collection_members(tx, tenant, col, &[])
            .await?
            .is_none()
        {
            return Err(Error::unprocessable("the shop has no such collection")
                .with_code("invalid_discount"));
        }
    }
    let sql = if insert {
        format!(
            "INSERT INTO discounts (id, tenant_id, code, kind, value, currency, \
             min_subtotal_minor, starts_at, ends_at, usage_limit, once_per_customer, active, \
             title, automatic, buy_quantity, get_quantity, collection_id) \
             VALUES ($2, $1, $3, $4, $5, $6, $7, $8::timestamptz, $9::timestamptz, $10, $11, $12, \
             $13, $14, $15, $16, $17) RETURNING {COLUMNS}"
        )
    } else {
        format!(
            "UPDATE discounts SET code = $3, kind = $4, value = $5, currency = $6, \
             min_subtotal_minor = $7, starts_at = $8::timestamptz, ends_at = $9::timestamptz, \
             usage_limit = $10, once_per_customer = $11, active = $12, title = $13, \
             automatic = $14, buy_quantity = $15, get_quantity = $16, collection_id = $17 \
             WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
        )
    };
    let dates = |s: &Option<String>| -> Result<Option<String>> {
        match s.as_deref() {
            Some(v) if v.len() > 40 || !v.contains('T') => {
                Err(Error::unprocessable(format!("{v} is not an RFC 3339 time"))
                    .with_code("invalid_discount"))
            }
            other => Ok(other.map(str::to_owned)),
        }
    };
    let row = sqlx::query_as::<_, Discount>(&sql)
        .bind(tenant.id())
        .bind(id)
        .bind(&c.code)
        .bind(&d.kind)
        .bind(if d.kind == "free_shipping" {
            0
        } else {
            d.value
        })
        .bind(&c.currency)
        .bind(d.min_subtotal_minor)
        .bind(dates(&d.starts_at)?)
        .bind(dates(&d.ends_at)?)
        .bind(d.usage_limit)
        .bind(d.once_per_customer)
        .bind(d.active.unwrap_or(true))
        .bind(&c.title)
        .bind(d.automatic)
        .bind(c.buy)
        .bind(c.get)
        .bind(collection)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("22007") => {
                Error::unprocessable("starts_at and ends_at are RFC 3339 times")
                    .with_code("invalid_discount")
            }
            _ => e.into(),
        })?;
    Ok(row)
}

/// The shop's discounts, newest first (`promotions:read`). `?q=`
/// searches codes and titles; filter with `?kind=`.
#[get("/api/discounts", tag = "promotions")]
async fn list_discounts(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Discount>>> {
    let mut tx = tenant.begin_for(&db, &PROMOTIONS_READ).await?;
    let page = TenantListing::new("discounts", COLUMNS)
        .sortable(["id", "code", "used_count"])
        .default_sort("-id")
        .search(["code", "title"])
        .filter_text("kind")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a discount code or an automatic discount (`promotions:manage`).
/// A code the shop already has is a 409. Publishes `discount.created`.
#[post("/api/discounts", tag = "promotions", status = 201)]
async fn create_discount(
    db: Db,
    tenant: Tenant,
    Valid(Json(d)): Valid<Json<DiscountInput>>,
) -> Result<(StatusCode, Json<Discount>)> {
    let mut tx = tenant.begin_for(&db, &PROMOTIONS_MANAGE).await?;
    let made = write(&mut tx, &tenant, DiscountId::new().as_str(), &d, true)
        .await?
        .ok_or_else(|| Error::internal("insert returned nothing"))?;
    tenant.publish(&mut tx, "discount.created", &made).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// One discount (`promotions:read`).
#[get("/api/discounts/{id}", tag = "promotions")]
async fn get_discount(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Discount>> {
    let id = DiscountId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PROMOTIONS_READ).await?;
    let d = sqlx::query_as::<_, Discount>(&format!(
        "SELECT {COLUMNS} FROM discounts WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    d.map(Json).ok_or_else(Error::not_found)
}

/// Replace a discount; its use count stays (`promotions:manage`).
/// Publishes `discount.updated`.
#[put("/api/discounts/{id}", tag = "promotions")]
async fn update_discount(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(d)): Valid<Json<DiscountInput>>,
) -> Result<Json<Discount>> {
    let id = DiscountId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PROMOTIONS_MANAGE).await?;
    let d = write(&mut tx, &tenant, id.as_str(), &d, false)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "discount.updated", &d).await?;
    tx.commit().await?;
    Ok(Json(d))
}

/// Delete a discount; orders keep what they saved (`promotions:manage`).
/// Publishes `discount.deleted`.
#[delete("/api/discounts/{id}", tag = "promotions", status = 204)]
async fn delete_discount(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = DiscountId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &PROMOTIONS_MANAGE).await?;
    let gone = sqlx::query("DELETE FROM discounts WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "discount.deleted",
            &serde_json::json!({ "id": id.as_str() }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A line of the order a discount would apply to.
#[derive(Debug, Clone)]
pub struct DiscountLine {
    pub product_id: Option<String>,
    pub unit_price_minor: i64,
    pub quantity: i64,
    /// Already taken off the line by another discount.
    pub off_minor: i64,
}

impl DiscountLine {
    fn worth(&self) -> i64 {
        (self.unit_price_minor * self.quantity - self.off_minor).max(0)
    }
}

/// The order a discount would apply to.
#[derive(Debug, Clone)]
pub struct DiscountContext<'a> {
    pub currency: &'a str,
    pub lines: &'a [DiscountLine],
    /// Shipping still to pay.
    pub shipping_minor: i64,
    pub email: &'a str,
    pub customer_id: Option<&'a str>,
}

/// A discount applied to an order.
#[model]
pub struct AppliedDiscount {
    pub discount_id: String,
    /// Null for an automatic discount.
    pub code: Option<String>,
    pub title: String,
    pub automatic: bool,
    pub kind: String,
    /// Off the goods.
    pub goods_minor: i64,
    /// Off shipping.
    pub shipping_minor: i64,
    /// Off each line, in the order of the lines.
    pub lines_minor: Vec<i64>,
}

#[derive(sqlx::FromRow)]
struct Candidate {
    id: String,
    code: Option<String>,
    title: String,
    automatic: bool,
    kind: String,
    value: i64,
    buy_quantity: Option<i64>,
    get_quantity: Option<i64>,
    collection_id: Option<String>,
    currency: Option<String>,
    min_subtotal_minor: Option<i64>,
    active: bool,
    in_dates: bool,
    left: bool,
    once_per_customer: bool,
}

const CANDIDATE: &str = "SELECT id, code, title, automatic, kind, value, buy_quantity, \
     get_quantity, collection_id, currency, min_subtotal_minor, active, \
     (starts_at IS NULL OR starts_at <= now()) AND (ends_at IS NULL OR ends_at > now()) AS in_dates, \
     (usage_limit IS NULL OR used_count < usage_limit) AS left, once_per_customer \
     FROM discounts WHERE tenant_id = $1";

/// What `d` takes off the order, or why it does not apply.
async fn evaluate(
    conn: &mut PgConnection,
    scope: &impl Scope,
    d: Candidate,
    ctx: &DiscountContext<'_>,
) -> Result<std::result::Result<AppliedDiscount, String>> {
    if !d.active || !d.in_dates {
        return Ok(Err("this code is not valid now".into()));
    }
    if !d.left {
        return Ok(Err("this code has been used up".into()));
    }
    if let Some(c) = d.currency.as_deref().filter(|c| *c != ctx.currency) {
        return Ok(Err(format!("this code is for orders in {c}")));
    }
    // The lines that count: all, or those of the collection.
    let counts: Vec<bool> = match &d.collection_id {
        None => vec![true; ctx.lines.len()],
        Some(col) => {
            let ids: Vec<String> = ctx
                .lines
                .iter()
                .filter_map(|l| l.product_id.clone())
                .collect();
            let members = soat_catalog::collection_members(conn, scope, col, &ids)
                .await?
                .unwrap_or_default();
            ctx.lines
                .iter()
                .map(|l| l.product_id.as_ref().is_some_and(|p| members.contains(p)))
                .collect()
        }
    };
    let worth: Vec<i64> = ctx
        .lines
        .iter()
        .zip(&counts)
        .map(|(l, c)| if *c { l.worth() } else { 0 })
        .collect();
    let subtotal: i64 = worth.iter().sum();
    if subtotal == 0 && d.kind != "free_shipping" {
        return Ok(Err(
            "this code does not apply to anything in the cart".into()
        ));
    }
    if let Some(min) = d.min_subtotal_minor.filter(|m| subtotal < *m) {
        return Ok(Err(format!("this code needs a subtotal of at least {min}")));
    }
    if d.once_per_customer {
        let used: (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM discount_redemptions WHERE tenant_id = $1 \
             AND discount_id = $2 AND (email = $3 OR (customer_id IS NOT NULL AND customer_id = $4)))",
        )
        .bind(scope.tenant_id())
        .bind(&d.id)
        .bind(ctx.email.trim().to_lowercase())
        .bind(ctx.customer_id)
        .fetch_one(&mut *conn)
        .await?;
        if used.0 {
            return Ok(Err("this code can be used once per customer".into()));
        }
    }
    let (lines_minor, shipping_minor) = if d.kind == "buy_x_get_y" {
        let units: Vec<(i64, i64)> = ctx
            .lines
            .iter()
            .zip(&worth)
            .map(|(l, w)| {
                let q = if *w > 0 { l.quantity.max(1) } else { 0 };
                (if q > 0 { w / q } else { 0 }, q)
            })
            .collect();
        let off = buy_x_get_y(
            &units,
            d.buy_quantity.unwrap_or(0),
            d.get_quantity.unwrap_or(0),
            d.value,
        );
        let off: Vec<i64> = off
            .into_iter()
            .zip(&worth)
            .map(|(o, w)| o.min(*w))
            .collect();
        if off.iter().all(|o| *o == 0) {
            let need = d.buy_quantity.unwrap_or(0) + d.get_quantity.unwrap_or(0);
            return Ok(Err(format!("this needs {need} items that it applies to")));
        }
        (off, 0)
    } else {
        let k =
            kind_of(&d.kind, d.value).ok_or_else(|| Error::internal("unknown discount kind"))?;
        let (goods, shipping) = amounts(k, subtotal, ctx.shipping_minor);
        (allocate(goods, &worth), shipping)
    };
    Ok(Ok(AppliedDiscount {
        discount_id: d.id,
        code: d.code,
        title: d.title,
        automatic: d.automatic,
        kind: d.kind,
        goods_minor: lines_minor.iter().sum(),
        shipping_minor,
        lines_minor,
    }))
}

/// What `code` takes off an order, or a 422 `invalid_discount` saying why
/// it does not apply. Run it in the shop's transaction.
pub async fn check(
    conn: &mut PgConnection,
    scope: &impl Scope,
    code: &str,
    ctx: &DiscountContext<'_>,
) -> Result<AppliedDiscount> {
    let no = |m: &str| Error::unprocessable(m.to_owned()).with_code("invalid_discount");
    let code = normalize_code(code).ok_or_else(|| no("no such discount code"))?;
    let d = sqlx::query_as::<_, Candidate>(&format!("{CANDIDATE} AND code = $2"))
        .bind(scope.tenant_id())
        .bind(&code)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| no("no such discount code"))?;
    evaluate(conn, scope, d, ctx).await?.map_err(|m| no(&m))
}

/// The automatic discount worth most for an order, if any applies. Run it
/// in the shop's transaction.
pub async fn automatic(
    conn: &mut PgConnection,
    scope: &impl Scope,
    ctx: &DiscountContext<'_>,
) -> Result<Option<AppliedDiscount>> {
    let found = sqlx::query_as::<_, Candidate>(&format!(
        "{CANDIDATE} AND automatic AND active ORDER BY id"
    ))
    .bind(scope.tenant_id())
    .fetch_all(&mut *conn)
    .await?;
    let mut best: Option<AppliedDiscount> = None;
    for d in found {
        if let Ok(a) = evaluate(conn, scope, d, ctx).await? {
            let worth = |a: &AppliedDiscount| a.goods_minor + a.shipping_minor;
            if worth(&a) > 0 && best.as_ref().is_none_or(|b| worth(&a) > worth(b)) {
                best = Some(a);
            }
        }
    }
    Ok(best)
}

/// Records that an order used a discount, in the order's transaction,
/// counting it against the limit. A limit reached meanwhile is a 409
/// `discount_used_up`. Publishes `discount.redeemed`.
pub async fn redeem(
    tx: &mut TenantTx,
    scope: &impl Scope,
    applied: &AppliedDiscount,
    order_id: &str,
    customer_id: Option<&str>,
    email: &str,
) -> Result<()> {
    let counted = sqlx::query(
        "UPDATE discounts SET used_count = used_count + 1 WHERE tenant_id = $1 AND id = $2 \
         AND (usage_limit IS NULL OR used_count < usage_limit)",
    )
    .bind(scope.tenant_id())
    .bind(&applied.discount_id)
    .execute(&mut **tx)
    .await?;
    if counted.rows_affected() == 0 {
        return Err(
            Error::conflict("this discount has just been used up").with_code("discount_used_up")
        );
    }
    let amount = applied.goods_minor + applied.shipping_minor;
    sqlx::query(
        "INSERT INTO discount_redemptions (tenant_id, discount_id, order_id, customer_id, email, \
         amount_minor) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(scope.tenant_id())
    .bind(&applied.discount_id)
    .bind(order_id)
    .bind(customer_id)
    .bind(email.trim().to_lowercase())
    .bind(amount)
    .execute(&mut **tx)
    .await?;
    scope
        .publish(
            tx,
            "discount.redeemed",
            &serde_json::json!({
                "discount_id": applied.discount_id,
                "code": applied.code,
                "title": applied.title,
                "automatic": applied.automatic,
                "order_id": order_id,
                "amount_minor": amount,
            }),
        )
        .await?;
    Ok(())
}
