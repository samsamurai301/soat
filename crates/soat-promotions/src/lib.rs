//! Promotions: discount codes.
//!
//! A code takes a percentage or a fixed amount off the goods, or makes
//! shipping free, between optional dates, above an optional minimum, up to
//! an optional number of uses (and once per customer if asked). Checkout
//! asks [`check`] what a code is worth for a cart and records its use with
//! [`redeem`] when the order is placed. Automatic discounts and buy X get
//! Y come in C14.

use soat_core::discount::{amounts, normalize_code, DiscountKind};
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
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[PROMOTIONS_READ, PROMOTIONS_MANAGE]
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

/// A discount code.
#[model]
#[derive(sqlx::FromRow)]
pub struct Discount {
    /// e.g. `dsc_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Unique in the shop, uppercase, e.g. `SUMMER10`.
    pub code: String,
    /// `percentage`, `fixed_amount` or `free_shipping`.
    pub kind: String,
    /// Basis points for a percentage (1000 = 10%), minor units for a
    /// fixed amount.
    pub value: i64,
    /// The currency of a fixed amount and of the minimum; null for any.
    pub currency: Option<String>,
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

const COLUMNS: &str = "id, code, kind, value, currency, min_subtotal_minor, \
     to_char(starts_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS starts_at, \
     to_char(ends_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS ends_at, \
     usage_limit, once_per_customer, used_count, active";

/// Body of creating or replacing a discount.
#[model]
pub struct DiscountInput {
    pub code: String,
    pub kind: String,
    #[serde(default)]
    pub value: i64,
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

fn checked(d: &DiscountInput) -> Result<(String, Option<String>)> {
    let bad = |m: String| Error::unprocessable(m).with_code("invalid_discount");
    let code = normalize_code(&d.code)
        .ok_or_else(|| bad("a code is 1 to 50 of A-Z, 0-9, - and _".into()))?;
    match d.kind.as_str() {
        "percentage" if !(1..=10_000).contains(&d.value) => {
            return Err(bad("a percentage is 1 to 10000 basis points".into()))
        }
        "fixed_amount" if d.value < 1 => return Err(bad("a fixed amount is at least 1".into())),
        "fixed_amount" if d.currency.is_none() => {
            return Err(bad("a fixed amount needs a currency".into()))
        }
        "percentage" | "fixed_amount" | "free_shipping" => {}
        k => {
            return Err(bad(format!(
                "{k} is not percentage, fixed_amount or free_shipping"
            )))
        }
    }
    let currency = d
        .currency
        .as_deref()
        .map(|c| Currency::parse(c).map(|c| c.as_str().to_owned()))
        .transpose()
        .map_err(|e| bad(e.to_string()))?;
    Ok((code, currency))
}

async fn write(
    tx: &mut TenantTx,
    tenant: &Tenant,
    id: &str,
    d: &DiscountInput,
    insert: bool,
) -> Result<Option<Discount>> {
    let (code, currency) = checked(d)?;
    let sql = if insert {
        format!(
            "INSERT INTO discounts (id, tenant_id, code, kind, value, currency, \
             min_subtotal_minor, starts_at, ends_at, usage_limit, once_per_customer, active) \
             VALUES ($2, $1, $3, $4, $5, $6, $7, $8::timestamptz, $9::timestamptz, $10, $11, $12) \
             RETURNING {COLUMNS}"
        )
    } else {
        format!(
            "UPDATE discounts SET code = $3, kind = $4, value = $5, currency = $6, \
             min_subtotal_minor = $7, starts_at = $8::timestamptz, ends_at = $9::timestamptz, \
             usage_limit = $10, once_per_customer = $11, active = $12 \
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
        .bind(&code)
        .bind(&d.kind)
        .bind(if d.kind == "free_shipping" {
            0
        } else {
            d.value
        })
        .bind(&currency)
        .bind(d.min_subtotal_minor)
        .bind(dates(&d.starts_at)?)
        .bind(dates(&d.ends_at)?)
        .bind(d.usage_limit)
        .bind(d.once_per_customer)
        .bind(d.active.unwrap_or(true))
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

/// The shop's discount codes, newest first (`promotions:read`). `?q=`
/// searches codes; filter with `?kind=`.
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
        .search(["code"])
        .filter_text("kind")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a discount code (`promotions:manage`). A code the shop already
/// has is a 409. Publishes `discount.created`.
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

/// The order a code would apply to.
#[derive(Debug, Clone)]
pub struct DiscountContext<'a> {
    pub currency: &'a str,
    pub subtotal_minor: i64,
    pub shipping_minor: i64,
    pub email: &'a str,
    pub customer_id: Option<&'a str>,
}

/// A code applied to an order.
#[model]
pub struct AppliedDiscount {
    pub discount_id: String,
    pub code: String,
    pub kind: String,
    /// Off the goods.
    pub goods_minor: i64,
    /// Off shipping.
    pub shipping_minor: i64,
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
    type Row = (
        String,
        String,
        i64,
        Option<String>,
        Option<i64>,
        bool,
        bool,
        bool,
        bool,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, kind, value, currency, min_subtotal_minor, active, \
         (starts_at IS NULL OR starts_at <= now()) AND (ends_at IS NULL OR ends_at > now()), \
         usage_limit IS NULL OR used_count < usage_limit, once_per_customer \
         FROM discounts WHERE tenant_id = $1 AND code = $2",
    )
    .bind(scope.tenant_id())
    .bind(&code)
    .fetch_optional(&mut *conn)
    .await?;
    let (id, kind, value, currency, min, active, in_dates, left, once) =
        row.ok_or_else(|| no("no such discount code"))?;
    if !active || !in_dates {
        return Err(no("this code is not valid now"));
    }
    if !left {
        return Err(no("this code has been used up"));
    }
    if currency.as_deref().is_some_and(|c| c != ctx.currency) {
        return Err(no(&format!(
            "this code is for orders in {}",
            currency.unwrap_or_default()
        )));
    }
    if let Some(min) = min.filter(|m| ctx.subtotal_minor < *m) {
        return Err(no(&format!("this code needs a subtotal of at least {min}")));
    }
    if once {
        let used: (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM discount_redemptions WHERE tenant_id = $1 \
             AND discount_id = $2 AND (email = $3 OR (customer_id IS NOT NULL AND customer_id = $4)))",
        )
        .bind(scope.tenant_id())
        .bind(&id)
        .bind(ctx.email.trim().to_lowercase())
        .bind(ctx.customer_id)
        .fetch_one(&mut *conn)
        .await?;
        if used.0 {
            return Err(no("this code can be used once per customer"));
        }
    }
    let k = kind_of(&kind, value).ok_or_else(|| Error::internal("unknown discount kind"))?;
    let (goods_minor, shipping_minor) = amounts(k, ctx.subtotal_minor, ctx.shipping_minor);
    Ok(AppliedDiscount {
        discount_id: id,
        code,
        kind,
        goods_minor,
        shipping_minor,
    })
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
            Error::conflict("this code has just been used up").with_code("discount_used_up")
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
                "order_id": order_id,
                "amount_minor": amount,
            }),
        )
        .await?;
    Ok(())
}
