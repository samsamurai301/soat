//! Point of sale: selling in a shop's own stores.
//!
//! A **register** (`/api/pos/registers`) is a till at one of the shop's
//! locations; what it sells leaves stock there, and its taxes are worked
//! out at the location's address. Staff open a **session** (a shift) with
//! the cash in the drawer, ring up **sales** (`POST
//! /api/pos/sessions/{id}/sales`) by item or SKU and take cash, card or
//! other tenders (cash may be more than due: the change is given back),
//! and close the session with the cash they count. Each sale is an order,
//! paid and handed over at once, so it shows in orders, stock, invoices
//! and reports like any other. The session's report gives the sales, the
//! totals by tender and the cash that should be in the drawer, against
//! what was counted.

use serde_json::json;
use soat_checkout::drafts::{self, DraftInput, DraftLineInput};
use soat_checkout::TaxProviders;
use soat_core::{id_kind, Currency, Permission};
use soat_module::Module;
use soat_orders::{Order, PostalAddress};
use soat_tenancy::{Scope, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

id_kind!(RegisterId, RegisterKind, "reg");
id_kind!(SessionId, SessionKind, "pss");

/// Ring up sales at an open register.
pub const POS_SELL: Permission =
    Permission::new("pos:sell", "Open and close registers and ring up sales").for_staff();
/// Set up registers.
pub const POS_MANAGE: Permission = Permission::new("pos:manage", "Set up registers").for_staff();

/// Most lines in one sale.
const MAX_LINES: usize = 200;

/// The point of sale module.
pub struct Pos;

impl Module for Pos {
    fn name(&self) -> &'static str {
        "pos"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "catalog",
            "inventory",
            "payments",
            "orders",
            "checkout",
        ]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[POS_SELL, POS_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_registers,
            create_register,
            update_register,
            open_session,
            get_session,
            close_session,
            quote_sale,
            ring_up,
            receipt,
        ))
    }
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_sale")
}

fn ts(column: &str, name: &str) -> String {
    format!("to_char({column} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS {name}")
}

// ---------------------------------------------------------------- registers

/// A till at one of the shop's locations.
#[model]
#[derive(sqlx::FromRow)]
pub struct Register {
    /// e.g. `reg_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub name: String,
    pub location_id: String,
    pub location_name: String,
    pub currency: String,
    pub active: bool,
    /// Its open session, if any.
    pub session_id: Option<String>,
}

fn register_columns() -> &'static str {
    "r.id, r.name, r.location_id, l.name AS location_name, r.currency, r.active, \
     (SELECT s.id FROM pos_sessions s WHERE s.tenant_id = r.tenant_id AND s.register_id = r.id \
      AND s.status = 'open') AS session_id"
}

#[model]
pub struct RegisterList {
    pub items: Vec<Register>,
}

/// A new register.
#[model]
pub struct NewRegister {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub location_id: String,
    /// Defaults to the shop's main currency.
    pub currency: Option<String>,
}

/// Changes to a register.
#[model]
pub struct RegisterChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    pub active: Option<bool>,
}

async fn register(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Register> {
    let id = RegisterId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {} FROM pos_registers r JOIN locations l ON l.id = r.location_id \
         WHERE r.tenant_id = $1 AND r.id = $2",
        register_columns()
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's registers (`pos:sell`).
#[get("/api/pos/registers", tag = "pos")]
async fn list_registers(db: Db, tenant: Tenant) -> Result<Json<RegisterList>> {
    let mut tx = tenant.begin_for(&db, &POS_SELL).await?;
    let items = sqlx::query_as(&format!(
        "SELECT {} FROM pos_registers r JOIN locations l ON l.id = r.location_id \
         WHERE r.tenant_id = $1 ORDER BY r.name, r.id LIMIT 200",
        register_columns()
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(RegisterList { items }))
}

/// Set up a register at a location (`pos:manage`). Publishes
/// `pos_register.created`.
#[post("/api/pos/registers", tag = "pos", status = 201)]
async fn create_register(
    db: Db,
    tenant: Tenant,
    Valid(Json(n)): Valid<Json<NewRegister>>,
) -> Result<(StatusCode, Json<Register>)> {
    let mut tx = tenant.begin_for(&db, &POS_MANAGE).await?;
    let location = soat_inventory::locations::load(&mut tx, &tenant, &n.location_id)
        .await
        .map_err(|_| invalid(format!("the shop has no location {}", n.location_id)))?;
    let currency = match n.currency.as_deref() {
        Some(c) => c.to_owned(),
        None => soat_catalog::storefront::main_currency(&mut tx, &tenant)
            .await?
            .unwrap_or_else(|| "EUR".to_owned()),
    };
    let currency = Currency::parse(&currency).map_err(|e| invalid(e.to_string()))?;
    let id = RegisterId::new();
    sqlx::query(
        "INSERT INTO pos_registers (id, tenant_id, name, location_id, currency) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(n.name.trim())
    .bind(&location.id)
    .bind(currency.as_str())
    .execute(&mut *tx)
    .await?;
    let r = register(&mut tx, &tenant, id.as_str()).await?;
    tenant.publish(&mut tx, "pos_register.created", &r).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(r)))
}

/// Rename a register or take it out of use (`pos:manage`). Publishes
/// `pos_register.updated`.
#[patch("/api/pos/registers/{id}", tag = "pos")]
async fn update_register(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<RegisterChange>>,
) -> Result<Json<Register>> {
    let mut tx = tenant.begin_for(&db, &POS_MANAGE).await?;
    let r = register(&mut tx, &tenant, &id).await?;
    sqlx::query(
        "UPDATE pos_registers SET name = COALESCE($3, name), active = COALESCE($4, active) \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&r.id)
    .bind(c.name.as_deref().map(str::trim))
    .bind(c.active)
    .execute(&mut *tx)
    .await?;
    let r = register(&mut tx, &tenant, &r.id).await?;
    tenant.publish(&mut tx, "pos_register.updated", &r).await?;
    tx.commit().await?;
    Ok(Json(r))
}

// ----------------------------------------------------------------- sessions

/// A shift at a register, with its report.
#[model]
pub struct Session {
    /// e.g. `pss_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub register_id: String,
    pub register_name: String,
    pub currency: String,
    /// `open` or `closed`.
    pub status: String,
    pub opened_by: Option<String>,
    pub opened_at: String,
    pub closed_by: Option<String>,
    pub closed_at: Option<String>,
    pub opening_cash_minor: i64,
    pub sales: i64,
    pub total_minor: i64,
    pub cash_minor: i64,
    pub card_minor: i64,
    pub other_minor: i64,
    /// Opening cash plus cash taken less change given.
    pub expected_cash_minor: i64,
    pub counted_cash_minor: Option<i64>,
    /// Counted less expected: below zero is cash missing.
    pub difference_minor: Option<i64>,
    pub note: String,
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    register_id: String,
    register_name: String,
    currency: String,
    status: String,
    opened_by: Option<String>,
    opened_at: String,
    closed_by: Option<String>,
    closed_at: Option<String>,
    opening_cash_minor: i64,
    counted_cash_minor: Option<i64>,
    note: String,
    location_id: String,
    sales: i64,
    total_minor: i64,
    cash_minor: i64,
    card_minor: i64,
    other_minor: i64,
    change_minor: i64,
}

async fn session_row(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
    lock: bool,
) -> Result<SessionRow> {
    let id = SessionId::parse(id).map_err(|_| Error::not_found())?;
    let row: Option<SessionRow> = sqlx::query_as(&format!(
        "SELECT s.id, s.register_id, r.name AS register_name, r.currency, s.status, s.opened_by, \
         {}, s.closed_by, {}, s.opening_cash_minor, s.counted_cash_minor, s.note, r.location_id, \
         (SELECT count(*) FROM pos_sales x WHERE x.tenant_id = s.tenant_id AND x.session_id = s.id) AS sales, \
         {} FROM pos_sessions s JOIN pos_registers r ON r.id = s.register_id \
         WHERE s.tenant_id = $1 AND s.id = $2{}",
        ts("s.opened_at", "opened_at"),
        ts("s.closed_at", "closed_at"),
        ["total_minor", "cash_minor", "card_minor", "other_minor", "change_minor"]
            .map(|c| format!(
                "COALESCE((SELECT sum(x.{c}) FROM pos_sales x WHERE x.tenant_id = s.tenant_id \
                 AND x.session_id = s.id), 0)::bigint AS {c}"
            ))
            .join(", "),
        if lock { " FOR UPDATE OF s" } else { "" }
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?;
    row.ok_or_else(Error::not_found)
}

fn shown(r: SessionRow) -> Session {
    let expected = r.opening_cash_minor + r.cash_minor - r.change_minor;
    Session {
        id: r.id,
        register_id: r.register_id,
        register_name: r.register_name,
        currency: r.currency,
        status: r.status,
        opened_by: r.opened_by,
        opened_at: r.opened_at,
        closed_by: r.closed_by,
        closed_at: r.closed_at,
        opening_cash_minor: r.opening_cash_minor,
        sales: r.sales,
        total_minor: r.total_minor,
        cash_minor: r.cash_minor - r.change_minor,
        card_minor: r.card_minor,
        other_minor: r.other_minor,
        expected_cash_minor: expected,
        counted_cash_minor: r.counted_cash_minor,
        difference_minor: r.counted_cash_minor.map(|c| c - expected),
        note: r.note,
    }
}

async fn session(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Session> {
    Ok(shown(session_row(conn, scope, id, false).await?))
}

/// Cash in the drawer.
#[model]
pub struct CashCount {
    #[validate(range(min = 0))]
    pub cash_minor: i64,
    #[validate(length(max = 1000))]
    pub note: Option<String>,
}

/// Open a session at a register with the cash in its drawer
/// (`pos:sell`); one open at a time (409 `session_open`). Publishes
/// `pos_session.opened`.
#[post("/api/pos/registers/{id}/sessions", tag = "pos", status = 201)]
async fn open_session(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CashCount>>,
) -> Result<(StatusCode, Json<Session>)> {
    let mut tx = tenant.begin_for(&db, &POS_SELL).await?;
    let r = register(&mut tx, &tenant, &id).await?;
    if !r.active {
        return Err(Error::conflict("the register is not in use").with_code("register_inactive"));
    }
    if r.session_id.is_some() {
        return Err(Error::conflict("the register has an open session").with_code("session_open"));
    }
    let sid = SessionId::new();
    sqlx::query(
        "INSERT INTO pos_sessions (id, tenant_id, register_id, opened_by, opening_cash_minor, note) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(sid.as_str())
    .bind(tenant.id())
    .bind(&r.id)
    .bind(tenant.actor_id())
    .bind(c.cash_minor)
    .bind(c.note.as_deref().unwrap_or("").trim())
    .execute(&mut *tx)
    .await?;
    let s = session(&mut tx, &tenant, sid.as_str()).await?;
    tenant.publish(&mut tx, "pos_session.opened", &s).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(s)))
}

/// A session and its report (`pos:sell`).
#[get("/api/pos/sessions/{id}", tag = "pos")]
async fn get_session(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Session>> {
    let mut tx = tenant.begin_for(&db, &POS_SELL).await?;
    let s = session(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Close a session with the cash counted in the drawer (`pos:sell`); the
/// report gives the difference to what was expected. Publishes
/// `pos_session.closed`.
#[post("/api/pos/sessions/{id}/close", tag = "pos")]
async fn close_session(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CashCount>>,
) -> Result<Json<Session>> {
    let mut tx = tenant.begin_for(&db, &POS_SELL).await?;
    let r = session_row(&mut tx, &tenant, &id, true).await?;
    if r.status != "open" {
        return Err(Error::conflict("the session is closed").with_code("session_closed"));
    }
    sqlx::query(
        "UPDATE pos_sessions SET status = 'closed', counted_cash_minor = $3, closed_by = $4, \
         closed_at = now(), note = CASE WHEN $5 = '' THEN note ELSE $5 END \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&r.id)
    .bind(c.cash_minor)
    .bind(tenant.actor_id())
    .bind(c.note.as_deref().unwrap_or("").trim())
    .execute(&mut *tx)
    .await?;
    let s = session(&mut tx, &tenant, &r.id).await?;
    tenant.publish(&mut tx, "pos_session.closed", &s).await?;
    tx.commit().await?;
    Ok(Json(s))
}

// -------------------------------------------------------------------- sales

/// A line of a sale: an item by id or SKU.
#[model]
pub struct SaleLine {
    pub item_id: Option<String>,
    pub sku: Option<String>,
    #[validate(range(min = 1, max = 10000))]
    pub quantity: i64,
    /// A price of the cashier's own, per unit.
    #[validate(range(min = 0))]
    pub unit_price_minor: Option<i64>,
}

/// What is paid.
#[model]
pub struct Tender {
    /// `cash`, `card` (on the store's terminal) or `other` (e.g. a voucher).
    pub kind: String,
    #[validate(range(min = 1))]
    pub amount_minor: i64,
}

/// A sale to ring up.
#[model]
pub struct NewSale {
    #[validate(length(min = 1, max = 200))]
    pub lines: Vec<SaleLine>,
    /// Off the goods.
    #[validate(range(min = 0))]
    pub discount_minor: Option<i64>,
    /// The buyer, when known.
    pub customer_id: Option<String>,
    /// For a receipt by email.
    #[validate(email)]
    pub email: Option<String>,
    /// Leave out to only work out the total.
    #[serde(default)]
    pub tenders: Vec<Tender>,
}

/// What a sale comes to.
#[model]
pub struct SaleQuote {
    pub currency: String,
    pub lines: Vec<serde_json::Value>,
    pub subtotal_minor: i64,
    pub discount_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
}

/// A sale rung up.
#[model]
pub struct Sale {
    pub order: Order,
    pub total_minor: i64,
    pub cash_minor: i64,
    pub card_minor: i64,
    pub other_minor: i64,
    /// Cash to give back.
    pub change_minor: i64,
}

/// The session's register's location as an order address.
async fn address(tx: &mut TenantTx, tenant: &Tenant, location: &str) -> Result<PostalAddress> {
    let l = soat_inventory::locations::load(tx, tenant, location).await?;
    Ok(PostalAddress {
        first_name: String::new(),
        last_name: String::new(),
        company: l.name.clone(),
        line1: if l.address_line1.trim().is_empty() {
            l.name.clone()
        } else {
            l.address_line1
        },
        line2: l.address_line2,
        postal_code: l.postal_code,
        city: if l.city.trim().is_empty() {
            l.name
        } else {
            l.city
        },
        region: String::new(),
        country: l.country,
        phone: String::new(),
    })
}

/// The draft of sale `s` in session `r`.
async fn input(
    tx: &mut TenantTx,
    tenant: &Tenant,
    r: &SessionRow,
    s: &NewSale,
) -> Result<DraftInput> {
    if s.lines.is_empty() || s.lines.len() > MAX_LINES {
        return Err(invalid(format!("a sale has 1 to {MAX_LINES} lines")));
    }
    let mut lines = Vec::with_capacity(s.lines.len());
    for l in &s.lines {
        let item = match (&l.item_id, &l.sku) {
            (Some(i), _) => i.clone(),
            (None, Some(sku)) => soat_catalog::item_by_sku(tx, tenant, sku.trim())
                .await?
                .map(|(i, _)| i)
                .ok_or_else(|| invalid(format!("no item has the SKU {sku}")))?,
            (None, None) => return Err(invalid("a line names an item_id or a sku")),
        };
        lines.push(DraftLineInput {
            item_id: item,
            quantity: l.quantity,
            unit_price_minor: l.unit_price_minor,
        });
    }
    let at = address(tx, tenant, &r.location_id).await?;
    Ok(DraftInput {
        customer_id: s.customer_id.clone(),
        email: s.email.clone(),
        currency: r.currency.clone(),
        lines,
        shipping_address: Some(at.clone()),
        billing_address: Some(at),
        vat_number: None,
        shipping_name: None,
        shipping_price_minor: None,
        discount_minor: s.discount_minor,
        note: Some(format!("Sold at {}", r.register_name)),
    })
}

fn open(r: &SessionRow) -> Result<()> {
    if r.status != "open" {
        return Err(Error::conflict("the session is closed").with_code("session_closed"));
    }
    Ok(())
}

/// What a sale would come to, without ringing it up (`pos:sell`).
#[post("/api/pos/sessions/{id}/quote", tag = "pos")]
async fn quote_sale(
    db: Db,
    tenant: Tenant,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
    Valid(Json(s)): Valid<Json<NewSale>>,
) -> Result<Json<SaleQuote>> {
    let mut tx = tenant.begin_for(&db, &POS_SELL).await?;
    let r = session_row(&mut tx, &tenant, &id, false).await?;
    open(&r)?;
    let draft_input = input(&mut tx, &tenant, &r, &s).await?;
    // A draft that is never kept: the transaction is rolled back.
    let d = drafts::create(&mut tx, &tenant, &taxes, &draft_input).await?;
    drop(tx);
    if !d.issues.is_empty() {
        return Err(invalid(d.issues.join("; ")));
    }
    Ok(Json(SaleQuote {
        currency: d.currency.clone(),
        lines: d
            .lines
            .iter()
            .map(|l| {
                json!({ "item_id": l.item_id, "sku": l.sku, "title": l.title,
                        "quantity": l.quantity, "unit_price_minor": l.unit_price_minor,
                        "total_minor": l.unit_price_minor * l.quantity - l.discount_minor })
            })
            .collect(),
        subtotal_minor: d.subtotal_minor,
        discount_minor: d.discount_minor,
        tax_minor: d.tax_minor,
        total_minor: d.total_minor,
    }))
}

/// How tenders pay `total`: (cash, card, other, change). Only cash can be
/// more than due; card and other may not pay more than is left.
///
/// ```
/// use soat_pos::{settle, Tender};
/// let t = |kind: &str, amount_minor| Tender { kind: kind.into(), amount_minor };
/// assert_eq!(settle(1850, &[t("cash", 2000)]), Ok((2000, 0, 0, 150)));
/// assert_eq!(settle(1850, &[t("card", 1000), t("cash", 1000)]), Ok((1000, 1000, 0, 150)));
/// assert!(settle(1850, &[t("cash", 1000)]).is_err());
/// assert!(settle(1850, &[t("card", 2000)]).is_err());
/// ```
///
/// # Errors
///
/// What is wrong, for a 422.
pub fn settle(total: i64, tenders: &[Tender]) -> std::result::Result<(i64, i64, i64, i64), String> {
    let (mut cash, mut card, mut other) = (0_i64, 0_i64, 0_i64);
    for t in tenders {
        if t.amount_minor <= 0 {
            return Err("a tender is more than 0".into());
        }
        let slot = match t.kind.as_str() {
            "cash" => &mut cash,
            "card" => &mut card,
            "other" => &mut other,
            k => return Err(format!("{k} is not cash, card or other")),
        };
        *slot = slot
            .checked_add(t.amount_minor)
            .ok_or_else(|| "a tender is too large".to_owned())?;
    }
    let exact = card + other;
    if exact > total {
        return Err("card and other tenders are more than is due".into());
    }
    let paid = exact + cash;
    if paid < total {
        return Err(format!("{} short", total - paid));
    }
    let change = paid - total;
    if change > cash {
        return Err("only cash gives change".into());
    }
    Ok((cash, card, other, change))
}

/// Ring up a sale (`pos:sell`): the order is placed, paid with the
/// tenders and handed over from the register's location, all at once.
/// Too little paid is a 422 `tender_short`; too little stock a 409
/// `insufficient_stock`. Publishes `order.placed`, `payment.created`,
/// `fulfillment.created`, `order.fulfilled` and `pos_sale.completed`.
#[post("/api/pos/sessions/{id}/sales", tag = "pos", status = 201)]
async fn ring_up(
    db: Db,
    tenant: Tenant,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
    Valid(Json(s)): Valid<Json<NewSale>>,
) -> Result<(StatusCode, Json<Sale>)> {
    let mut tx = tenant.begin_for(&db, &POS_SELL).await?;
    let r = session_row(&mut tx, &tenant, &id, true).await?;
    open(&r)?;
    let draft_input = input(&mut tx, &tenant, &r, &s).await?;
    let (placed, _token) = drafts::sell_at_counter(&mut tx, &tenant, &taxes, &draft_input).await?;
    let (cash, card, other, change) = settle(placed.total_minor, &s.tenders)
        .map_err(|m| Error::unprocessable(m).with_code("tender_short"))?;
    for (kind, amount) in [("cash", cash - change), ("card", card), ("other", other)] {
        if amount > 0 {
            soat_payments::record(
                &mut tx,
                &tenant,
                &placed.id,
                "pos",
                &format!("{}:{kind}", placed.id),
                amount,
                &placed.currency,
            )
            .await?;
        }
    }
    soat_orders::sync_payments(&mut tx, &tenant, &placed.id).await?;
    let order = if placed.lines.is_empty() {
        placed
    } else {
        soat_orders::fulfillments::hand_over(&mut tx, &tenant, &placed.id, &r.location_id).await?
    };
    sqlx::query(
        "INSERT INTO pos_sales (tenant_id, order_id, session_id, number, total_minor, cash_minor, \
         card_minor, other_minor, change_minor, sold_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(tenant.id())
    .bind(&order.id)
    .bind(&r.id)
    .bind(order.number)
    .bind(order.total_minor)
    .bind(cash)
    .bind(card)
    .bind(other)
    .bind(change)
    .bind(tenant.actor_id())
    .execute(&mut *tx)
    .await?;
    let sale = Sale {
        total_minor: order.total_minor,
        order,
        cash_minor: cash,
        card_minor: card,
        other_minor: other,
        change_minor: change,
    };
    tenant
        .publish(
            &mut tx,
            "pos_sale.completed",
            &json!({ "order_id": sale.order.id, "session_id": r.id,
                     "register_id": r.register_id, "total_minor": sale.total_minor,
                     "cash_minor": cash, "card_minor": card, "other_minor": other,
                     "change_minor": change }),
        )
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(sale)))
}

/// A sale's receipt.
#[model]
pub struct Receipt {
    pub order: Order,
    pub register_name: String,
    pub location_name: String,
    pub cash_minor: i64,
    pub card_minor: i64,
    pub other_minor: i64,
    pub change_minor: i64,
    pub sold_at: String,
}

/// The receipt of a sale rung up at a register (`pos:sell`).
#[get("/api/pos/sales/{order_id}", tag = "pos")]
async fn receipt(db: Db, tenant: Tenant, Path(order_id): Path<String>) -> Result<Json<Receipt>> {
    let mut tx = tenant.begin_for(&db, &POS_SELL).await?;
    type Row = (String, String, i64, i64, i64, i64, String);
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT r.name, l.name, x.cash_minor, x.card_minor, x.other_minor, x.change_minor, {} \
         FROM pos_sales x JOIN pos_sessions s ON s.id = x.session_id \
         JOIN pos_registers r ON r.id = s.register_id JOIN locations l ON l.id = r.location_id \
         WHERE x.tenant_id = $1 AND x.order_id = $2",
        ts("x.created_at", "sold_at")
    ))
    .bind(tenant.id())
    .bind(&order_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (register_name, location_name, cash, card, other, change, sold_at) =
        row.ok_or_else(Error::not_found)?;
    let order = soat_orders::load(&mut tx, &tenant, &order_id)
        .await?
        .ok_or_else(Error::not_found)?;
    tx.commit().await?;
    Ok(Json(Receipt {
        order,
        register_name,
        location_name,
        cash_minor: cash,
        card_minor: card,
        other_minor: other,
        change_minor: change,
        sold_at,
    }))
}
