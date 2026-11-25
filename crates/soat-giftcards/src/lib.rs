//! Gift cards and store credit.
//!
//! Both are a balance in one currency that buyers spend at checkout. A
//! gift card is found by its code: staff issue one (the code is shown
//! once; only a hash is kept) and anyone holding the code can spend it.
//! Store credit belongs to one customer (one balance per currency) and is
//! spent by that customer when signed in. Every change is a transaction
//! in the card's ledger.
//!
//! Checkout spends a balance with [`redeem`]: it takes the amount off and
//! records it as a paid payment of the order (provider `gift_card` or
//! `store_credit`), so the order's financial status counts it. Refunding
//! that payment puts the money back on the card.
//!
//! A shop sells gift cards as ordinary products tagged [`GIFT_CARD_TAG`]
//! (best marked as not shipping, so the order line is delivered at once).
//! When such an order is paid, one card per unit is issued for the line's
//! unit price and `gift_card.issued` is published; the code is kept only
//! until [`issued_mail`] hands it to the email that sends it to the buyer.

use std::sync::Arc;

use serde_json::json;
use soat_core::{id_kind, Currency, Permission};
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_tenancy::{EventShop, Scope, Shop, Tenant, TenantListing, TenantTx};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::Resources;

id_kind!(GiftCardId, GiftCardKind, "gft");
id_kind!(GiftCardTxnId, GiftCardTxnKind, "gtx");

/// See gift cards, store credit and their ledgers.
pub const GIFTCARDS_READ: Permission =
    Permission::new("giftcards:read", "See gift cards and store credit").for_staff();
/// Issue gift cards and store credit, change and adjust them.
pub const GIFTCARDS_MANAGE: Permission = Permission::new(
    "giftcards:manage",
    "Issue gift cards and store credit, and change their balances",
)
.for_staff();

/// The gift cards module.
pub struct GiftCards;

impl Module for GiftCards {
    fn name(&self) -> &'static str {
        "giftcards"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "catalog", "customers", "payments"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[GIFTCARDS_READ, GIFTCARDS_MANAGE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(RefundToCard), Arc::new(SellCards)]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_cards,
            create_card,
            get_card,
            update_card,
            adjust_card,
            check_balance,
            my_store_credit,
        ))
    }
}

/// A gift card or a customer's store credit.
#[model]
#[derive(sqlx::FromRow)]
pub struct GiftCard {
    /// e.g. `gft_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// `gift_card` or `store_credit`.
    pub kind: String,
    /// The code's last four characters, e.g. `7KQ2`; empty for store
    /// credit.
    pub last_chars: String,
    pub currency: String,
    pub initial_minor: i64,
    pub balance_minor: i64,
    /// The customer it belongs to: always for store credit, optionally
    /// for a gift card.
    pub customer_id: Option<String>,
    /// RFC 3339; null for never.
    pub expires_at: Option<String>,
    pub note: String,
    pub active: bool,
    /// The order that bought it, for a gift card sold as a product.
    pub order_id: Option<String>,
    pub created_at: String,
}

const COLUMNS: &str = "id, kind, last_chars, currency, initial_minor, balance_minor, customer_id, \
     to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at, \
     note, active, order_id, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

/// A change of a balance.
#[model]
#[derive(sqlx::FromRow)]
pub struct GiftCardTransaction {
    pub id: String,
    /// `issued`, `adjusted`, `redeemed` or `refunded`.
    pub kind: String,
    /// Positive adds to the balance, negative takes from it.
    pub amount_minor: i64,
    /// The balance after it.
    pub balance_minor: i64,
    pub order_id: Option<String>,
    pub note: String,
    pub created_at: String,
}

const TXN_COLUMNS: &str = "id, kind, amount_minor, balance_minor, order_id, note, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

/// A card and its ledger, newest first.
#[model]
pub struct GiftCardDetail {
    #[serde(flatten)]
    pub card: GiftCard,
    pub transactions: Vec<GiftCardTransaction>,
}

/// A card just issued, with its code: shown this once.
#[model]
pub struct IssuedGiftCard {
    #[serde(flatten)]
    pub card: GiftCard,
    /// The whole code, e.g. `ABCD-EFGH-JKLM-NPQR`; null for store credit.
    pub code: Option<String>,
}

/// Body of issuing a gift card or store credit.
#[model]
pub struct NewGiftCard {
    /// `gift_card` (default) or `store_credit`.
    pub kind: Option<String>,
    /// The starting balance.
    #[validate(range(min = 0, max = 100_000_000))]
    pub amount_minor: i64,
    pub currency: String,
    /// A code of your own (8 to 32 letters and digits); one is made up
    /// when left out. Gift cards only.
    pub code: Option<String>,
    /// Required for store credit. A customer has one store credit per
    /// currency: add to it with an adjustment (409 `store_credit_exists`).
    pub customer_id: Option<String>,
    /// RFC 3339, e.g. `2027-12-31T23:59:59Z`.
    pub expires_at: Option<String>,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

/// Body of changing a card; absent fields stay as they are.
#[model]
pub struct GiftCardChange {
    /// RFC 3339; an empty string removes the end.
    pub expires_at: Option<String>,
    #[validate(length(max = 500))]
    pub note: Option<String>,
    /// A disabled card cannot be spent.
    pub active: Option<bool>,
}

/// Body of an adjustment.
#[model]
pub struct Adjustment {
    /// Positive adds, negative takes away; never below zero.
    #[validate(range(min = -100_000_000, max = 100_000_000))]
    pub amount_minor: i64,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

/// Body of checking a gift card's balance.
#[model]
pub struct BalanceQuery {
    #[validate(length(max = 64))]
    pub code: String,
}

/// What a gift card holds, for the buyer.
#[model]
pub struct Balance {
    pub last_chars: String,
    pub balance_minor: i64,
    pub currency: String,
    pub expires_at: Option<String>,
}

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// A code as people type it: uppercase letters and digits only.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A new code of 16 characters with no look-alikes, in groups of four.
fn new_code() -> Result<String> {
    let token = new_token()?;
    let chars: Vec<char> = token
        .bytes()
        .take(16)
        .map(|b| char::from(ALPHABET[usize::from(b) % ALPHABET.len()]))
        .collect();
    Ok(chars
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-"))
}

fn last_chars(normalized: &str) -> String {
    normalized
        .chars()
        .skip(normalized.len().saturating_sub(4))
        .collect()
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_gift_card")
}

fn time(s: Option<&str>) -> Result<Option<String>> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) if v.len() > 40 || !v.contains('T') => {
            Err(invalid(format!("{v} is not an RFC 3339 time")))
        }
        other => Ok(other.map(str::to_owned)),
    }
}

async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Option<GiftCard>> {
    let Ok(id) = GiftCardId::parse(id) else {
        return Ok(None);
    };
    Ok(sqlx::query_as::<_, GiftCard>(&format!(
        "SELECT {COLUMNS} FROM gift_cards WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?)
}

async fn detail(
    conn: &mut PgConnection,
    scope: &impl Scope,
    card: GiftCard,
) -> Result<GiftCardDetail> {
    let transactions = sqlx::query_as::<_, GiftCardTransaction>(&format!(
        "SELECT {TXN_COLUMNS} FROM gift_card_transactions \
         WHERE tenant_id = $1 AND gift_card_id = $2 ORDER BY id DESC LIMIT 200"
    ))
    .bind(scope.tenant_id())
    .bind(&card.id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(GiftCardDetail { card, transactions })
}

/// Moves a locked card's balance by `amount` and records why. Answers the
/// card after it.
#[allow(clippy::too_many_arguments)]
async fn ledger(
    tx: &mut TenantTx,
    scope: &impl Scope,
    card: &str,
    kind: &str,
    amount: i64,
    order_id: Option<&str>,
    refund_id: Option<&str>,
    note: &str,
) -> Result<(GiftCard, String)> {
    let card: GiftCard = sqlx::query_as(&format!(
        "UPDATE gift_cards SET balance_minor = balance_minor + $3 \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(scope.tenant_id())
    .bind(card)
    .bind(amount)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23514") => {
            Error::unprocessable("the balance cannot go below zero")
                .with_code("insufficient_balance")
        }
        _ => e.into(),
    })?;
    let id = GiftCardTxnId::new();
    sqlx::query(
        "INSERT INTO gift_card_transactions (id, tenant_id, gift_card_id, kind, amount_minor, \
         balance_minor, order_id, refund_id, note) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id.as_str())
    .bind(scope.tenant_id())
    .bind(&card.id)
    .bind(kind)
    .bind(amount)
    .bind(card.balance_minor)
    .bind(order_id)
    .bind(refund_id)
    .bind(note.trim())
    .execute(&mut **tx)
    .await?;
    Ok((card, id.to_string()))
}

/// Gift cards and store credit, newest first (`giftcards:read`). Filter
/// with `?kind=` or `?customer_id=`; `?q=` searches the last characters
/// of codes and notes.
#[get("/api/gift-cards", tag = "giftcards")]
async fn list_cards(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<GiftCard>>> {
    let mut tx = tenant.begin_for(&db, &GIFTCARDS_READ).await?;
    let page = TenantListing::new("gift_cards", COLUMNS)
        .sortable(["id", "balance_minor"])
        .default_sort("-id")
        .search(["last_chars", "note"])
        .filter_text("kind")
        .filter_text("customer_id")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Issue a gift card or store credit (`giftcards:manage`). A gift card's
/// code is in this answer only. Publishes `gift_card.created` (without
/// the code).
#[post("/api/gift-cards", tag = "giftcards", status = 201)]
async fn create_card(
    db: Db,
    tenant: Tenant,
    Valid(Json(n)): Valid<Json<NewGiftCard>>,
) -> Result<(StatusCode, Json<IssuedGiftCard>)> {
    let currency = Currency::parse(&n.currency).map_err(|e| invalid(e.to_string()))?;
    let kind = n.kind.as_deref().unwrap_or("gift_card");
    let mut tx = tenant.begin_for(&db, &GIFTCARDS_MANAGE).await?;
    let customer = n
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    if let Some(c) = customer {
        soat_customers::load(&mut tx, &tenant, c)
            .await
            .map_err(|_| invalid("the shop has no such customer"))?;
    }
    let code = match (kind, n.code.as_deref().map(normalize)) {
        ("gift_card", Some(c)) if (8..=32).contains(&c.len()) => Some(c),
        ("gift_card", Some(_)) => return Err(invalid("a code is 8 to 32 letters and digits")),
        ("gift_card", None) => Some(new_code()?),
        ("store_credit", None) if customer.is_some() => None,
        ("store_credit", None) => return Err(invalid("store credit needs a customer_id")),
        ("store_credit", Some(_)) => return Err(invalid("store credit has no code")),
        (k, _) => return Err(invalid(format!("{k} is not gift_card or store_credit"))),
    };
    let normalized = code.as_deref().map(normalize);
    let row = sqlx::query_as::<_, GiftCard>(&format!(
        "INSERT INTO gift_cards (id, tenant_id, kind, code_hash, last_chars, currency, \
         initial_minor, balance_minor, customer_id, expires_at, note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $9::timestamptz, $10) RETURNING {COLUMNS}"
    ))
    .bind(GiftCardId::new().as_str())
    .bind(tenant.id())
    .bind(kind)
    .bind(normalized.as_deref().map(hash_token))
    .bind(normalized.as_deref().map(last_chars).unwrap_or_default())
    .bind(currency.as_str())
    .bind(n.amount_minor)
    .bind(customer)
    .bind(time(n.expires_at.as_deref())?)
    .bind(n.note.as_deref().unwrap_or("").trim())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            if kind == "store_credit" {
                Error::conflict("the customer has store credit in this currency; adjust it")
                    .with_code("store_credit_exists")
            } else {
                Error::conflict("the shop has a gift card with this code").with_code("code_taken")
            }
        }
        sqlx::Error::Database(db) if db.code().as_deref() == Some("22007") => {
            invalid("expires_at is an RFC 3339 time")
        }
        _ => e.into(),
    })?;
    let card = if n.amount_minor > 0 {
        ledger(
            &mut tx,
            &tenant,
            &row.id,
            "issued",
            n.amount_minor,
            None,
            None,
            "",
        )
        .await?
        .0
    } else {
        row
    };
    tenant.publish(&mut tx, "gift_card.created", &card).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(IssuedGiftCard { card, code })))
}

/// A gift card or store credit with its ledger (`giftcards:read`).
#[get("/api/gift-cards/{id}", tag = "giftcards")]
async fn get_card(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<GiftCardDetail>> {
    let mut tx = tenant.begin_for(&db, &GIFTCARDS_READ).await?;
    let card = load(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let d = detail(&mut tx, &tenant, card).await?;
    tx.commit().await?;
    Ok(Json(d))
}

/// Change a card's end, note, or disable it (`giftcards:manage`).
/// Publishes `gift_card.updated`.
#[patch("/api/gift-cards/{id}", tag = "giftcards")]
async fn update_card(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<GiftCardChange>>,
) -> Result<Json<GiftCard>> {
    let id = GiftCardId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &GIFTCARDS_MANAGE).await?;
    let clear = c.expires_at.as_deref().is_some_and(|e| e.trim().is_empty());
    let card = sqlx::query_as::<_, GiftCard>(&format!(
        "UPDATE gift_cards SET \
         expires_at = CASE WHEN $3 THEN NULL ELSE COALESCE($4::timestamptz, expires_at) END, \
         note = COALESCE($5, note), active = COALESCE($6, active) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .bind(clear)
    .bind(time(c.expires_at.as_deref())?)
    .bind(c.note.as_deref().map(str::trim))
    .bind(c.active)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("22007") => {
            invalid("expires_at is an RFC 3339 time")
        }
        _ => e.into(),
    })?
    .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "gift_card.updated", &card).await?;
    tx.commit().await?;
    Ok(Json(card))
}

/// Add to or take from a balance (`giftcards:manage`), e.g. to give a
/// customer store credit for a complaint. Below zero is a 422
/// `insufficient_balance`. Publishes `gift_card.adjusted`.
#[post("/api/gift-cards/{id}/adjustments", tag = "giftcards")]
async fn adjust_card(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(a)): Valid<Json<Adjustment>>,
) -> Result<Json<GiftCardDetail>> {
    if a.amount_minor == 0 {
        return Err(invalid("an adjustment is not zero"));
    }
    let mut tx = tenant.begin_for(&db, &GIFTCARDS_MANAGE).await?;
    let card = load(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let note = a.note.as_deref().unwrap_or("");
    let (card, _) = ledger(
        &mut tx,
        &tenant,
        &card.id,
        "adjusted",
        a.amount_minor,
        None,
        None,
        note,
    )
    .await?;
    tenant
        .publish(
            &mut tx,
            "gift_card.adjusted",
            &json!({ "id": card.id, "amount_minor": a.amount_minor, "balance_minor": card.balance_minor, "note": note }),
        )
        .await?;
    let d = detail(&mut tx, &tenant, card).await?;
    tx.commit().await?;
    Ok(Json(d))
}

/// What a gift card holds, for anyone with its code. An unknown or
/// disabled code is a 404.
#[post("/api/storefront/gift-cards/balance", tag = "storefront")]
async fn check_balance(
    db: Db,
    shop: Shop,
    Valid(Json(q)): Valid<Json<BalanceQuery>>,
) -> Result<Json<Balance>> {
    let mut tx = shop.begin(&db).await?;
    let card = by_code(&mut tx, &shop, &q.code)
        .await?
        .filter(|c| c.active)
        .ok_or_else(Error::not_found)?;
    tx.commit().await?;
    Ok(Json(Balance {
        last_chars: card.last_chars,
        balance_minor: card.balance_minor,
        currency: card.currency,
        expires_at: card.expires_at,
    }))
}

/// The signed-in customer's store credit, per currency.
#[get("/api/storefront/account/store-credit", tag = "storefront")]
async fn my_store_credit(
    db: Db,
    signed_in: soat_customers::accounts::SignedIn,
) -> Result<Json<Vec<Balance>>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let cards = sqlx::query_as::<_, GiftCard>(&format!(
        "SELECT {COLUMNS} FROM gift_cards WHERE tenant_id = $1 AND customer_id = $2 \
         AND kind = 'store_credit' AND active ORDER BY currency"
    ))
    .bind(signed_in.shop.id())
    .bind(&customer)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(
        cards
            .into_iter()
            .map(|c| Balance {
                last_chars: c.last_chars,
                balance_minor: c.balance_minor,
                currency: c.currency,
                expires_at: c.expires_at,
            })
            .collect(),
    ))
}

async fn by_code(
    conn: &mut PgConnection,
    scope: &impl Scope,
    code: &str,
) -> Result<Option<GiftCard>> {
    let code = normalize(code);
    if code.len() < 8 {
        return Ok(None);
    }
    Ok(sqlx::query_as::<_, GiftCard>(&format!(
        "SELECT {COLUMNS} FROM gift_cards WHERE tenant_id = $1 AND code_hash = $2"
    ))
    .bind(scope.tenant_id())
    .bind(hash_token(&code))
    .fetch_optional(&mut *conn)
    .await?)
}

/// Why `card` cannot be spent on an order in `currency`, if it cannot.
async fn unusable(
    conn: &mut PgConnection,
    scope: &impl Scope,
    card: &GiftCard,
    currency: &str,
) -> Result<Option<String>> {
    if !card.active {
        return Ok(Some("this gift card is disabled".into()));
    }
    let expired: (bool,) = sqlx::query_as(
        "SELECT COALESCE(expires_at <= now(), FALSE) FROM gift_cards WHERE tenant_id = $1 AND id = $2",
    )
    .bind(scope.tenant_id())
    .bind(&card.id)
    .fetch_one(&mut *conn)
    .await?;
    if expired.0 {
        return Ok(Some("this gift card has expired".into()));
    }
    if card.currency != currency {
        return Ok(Some(format!("this gift card is in {}", card.currency)));
    }
    if card.balance_minor == 0 {
        return Ok(Some("this gift card has nothing left on it".into()));
    }
    Ok(None)
}

/// The gift card with `code`, if it can pay for an order in `currency`;
/// otherwise a 422 `invalid_gift_card` saying why. Run it in the shop's
/// transaction.
pub async fn find(
    conn: &mut PgConnection,
    scope: &impl Scope,
    code: &str,
    currency: &str,
) -> Result<GiftCard> {
    let card = by_code(conn, scope, code)
        .await?
        .ok_or_else(|| invalid("no such gift card"))?;
    match unusable(conn, scope, &card, currency).await? {
        Some(why) => Err(invalid(why)),
        None => Ok(card),
    }
}

/// Gift card `id` as it is now and why it cannot pay for an order in
/// `currency`, if it cannot. `None` for an unknown card.
pub async fn usable(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
    currency: &str,
) -> Result<Option<(GiftCard, Option<String>)>> {
    let Some(card) = load(conn, scope, id).await? else {
        return Ok(None);
    };
    let why = unusable(conn, scope, &card, currency).await?;
    Ok(Some((card, why)))
}

/// The customer's store credit in `currency` that can be spent now.
pub async fn store_credit(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer_id: &str,
    currency: &str,
) -> Result<Option<GiftCard>> {
    Ok(sqlx::query_as::<_, GiftCard>(&format!(
        "SELECT {COLUMNS} FROM gift_cards WHERE tenant_id = $1 AND customer_id = $2 \
         AND currency = $3 AND kind = 'store_credit' AND active AND balance_minor > 0 \
         AND (expires_at IS NULL OR expires_at > now())"
    ))
    .bind(scope.tenant_id())
    .bind(customer_id)
    .bind(currency)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Spends `amount` of card `id` on order `order_id`, in the order's
/// transaction: takes it off the balance and records it as a paid
/// payment of the order. A card that changed meanwhile (spent, disabled,
/// expired) is a 409 `gift_card_changed`. Publishes `gift_card.redeemed`
/// and the payment's events.
pub async fn redeem(
    tx: &mut TenantTx,
    scope: &impl Scope,
    id: &str,
    amount: i64,
    order_id: &str,
    currency: &str,
) -> Result<()> {
    let changed = || {
        Error::conflict("a gift card or store credit changed; check out again")
            .with_code("gift_card_changed")
    };
    let locked: Option<(String, bool)> = sqlx::query_as(
        "SELECT kind, active AND (expires_at IS NULL OR expires_at > now()) AND balance_minor >= $3 \
         AND currency = $4 FROM gift_cards WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .bind(amount)
    .bind(currency)
    .fetch_optional(&mut **tx)
    .await?;
    let (kind, ok) = locked.ok_or_else(changed)?;
    if !ok {
        return Err(changed());
    }
    let (card, txn) = ledger(tx, scope, id, "redeemed", -amount, Some(order_id), None, "").await?;
    soat_payments::record(tx, scope, order_id, &kind, &txn, amount, currency).await?;
    scope
        .publish(
            tx,
            "gift_card.redeemed",
            &json!({ "id": card.id, "order_id": order_id, "amount_minor": amount, "balance_minor": card.balance_minor }),
        )
        .await?;
    Ok(())
}

/// The product tag that makes a product a gift card.
pub const GIFT_CARD_TAG: &str = "gift-card";

/// A gift card sold as a product, ready to email to its buyer.
#[derive(Debug, Clone)]
pub struct IssuedMail {
    pub card: GiftCard,
    pub email: String,
    pub code: String,
}

/// The email for gift card `id` sold with an order, once: the code is
/// cleared as it is read. None when it has been read before.
///
/// # Errors
///
/// Database errors.
pub async fn issued_mail(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
) -> Result<Option<IssuedMail>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "WITH old AS (SELECT id, unsent_code, recipient_email FROM gift_cards \
         WHERE tenant_id = $1 AND id = $2 AND unsent_code IS NOT NULL FOR UPDATE) \
         UPDATE gift_cards SET unsent_code = NULL FROM old WHERE gift_cards.id = old.id \
         RETURNING old.unsent_code, COALESCE(old.recipient_email, '')",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((code, email)) = row else {
        return Ok(None);
    };
    let card = load(conn, scope, id)
        .await?
        .ok_or_else(|| Error::internal("gift card vanished"))?;
    Ok(Some(IssuedMail { card, email, code }))
}

/// Issues the gift cards a paid order bought.
struct SellCards;

impl Subscriber for SellCards {
    fn name(&self) -> &'static str {
        "giftcards-sold"
    }

    fn wants(&self, kind: &str) -> bool {
        kind == "order.paid"
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let order = &event.payload;
            let (Some(order_id), Some(currency)) =
                (order["id"].as_str(), order["currency"].as_str())
            else {
                return Ok(());
            };
            let email = order["email"].as_str().unwrap_or_default();
            let lines = order["lines"].as_array().cloned().unwrap_or_default();
            let pool: PgPool = res.require()?;
            let scope = EventShop(event.tenant_id.clone());
            let mut tx = scope.begin_on(&pool).await?;
            for line in &lines {
                let (Some(line_id), Some(product), Some(quantity), Some(price)) = (
                    line["id"].as_str(),
                    line["product_id"].as_str(),
                    line["quantity"].as_i64(),
                    line["unit_price_minor"].as_i64(),
                ) else {
                    continue;
                };
                let tags = soat_catalog::product_tags(&mut tx, &scope, product).await?;
                if price <= 0 || !tags.is_some_and(|t| t.iter().any(|t| t == GIFT_CARD_TAG)) {
                    continue;
                }
                for unit in 1..=quantity.clamp(0, 100) {
                    let code = new_code()?;
                    let normalized = normalize(&code);
                    let made: Option<(String,)> = sqlx::query_as(
                        "INSERT INTO gift_cards (id, tenant_id, kind, code_hash, last_chars, \
                         currency, initial_minor, balance_minor, order_id, order_line_id, \
                         line_unit, recipient_email, unsent_code) \
                         VALUES ($1, $2, 'gift_card', $3, $4, $5, $6, 0, $7, $8, $9, $10, $11) \
                         ON CONFLICT (tenant_id, order_line_id, line_unit) \
                         WHERE order_line_id IS NOT NULL DO NOTHING RETURNING id",
                    )
                    .bind(GiftCardId::new().as_str())
                    .bind(&event.tenant_id)
                    .bind(hash_token(&normalized))
                    .bind(last_chars(&normalized))
                    .bind(currency)
                    .bind(price)
                    .bind(order_id)
                    .bind(line_id)
                    .bind(i32::try_from(unit).unwrap_or(i32::MAX))
                    .bind(email)
                    .bind(&code)
                    .fetch_optional(&mut *tx)
                    .await?;
                    let Some((id,)) = made else {
                        continue; // issued before: the event is a retry
                    };
                    let (card, _) = ledger(
                        &mut tx,
                        &scope,
                        &id,
                        "issued",
                        price,
                        Some(order_id),
                        None,
                        "",
                    )
                    .await?;
                    scope.publish(&mut tx, "gift_card.issued", &card).await?;
                }
            }
            tx.commit().await?;
            Ok(())
        })
    }
}

/// Puts refunded gift card and store credit payments back on the card.
struct RefundToCard;

impl Subscriber for RefundToCard {
    fn name(&self) -> &'static str {
        "giftcards"
    }

    fn wants(&self, kind: &str) -> bool {
        kind == "payment.refunded"
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let (Some(refund), Some(payment), Some(amount)) = (
                event.payload["id"].as_str(),
                event.payload["payment_id"].as_str(),
                event.payload["amount_minor"].as_i64(),
            ) else {
                return Ok(());
            };
            let pool: PgPool = res.require()?;
            let scope = EventShop(event.tenant_id.clone());
            let mut tx = scope.begin_on(&pool).await?;
            let Some(p) = soat_payments::get(&mut tx, &scope, payment).await? else {
                return Ok(());
            };
            if !matches!(p.provider.as_str(), "gift_card" | "store_credit") {
                return Ok(());
            }
            let card: Option<(String,)> = sqlx::query_as(
                "SELECT gift_card_id FROM gift_card_transactions WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&event.tenant_id)
            .bind(p.provider_ref.as_deref().unwrap_or_default())
            .fetch_optional(&mut *tx)
            .await?;
            let done: (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM gift_card_transactions WHERE refund_id = $1)",
            )
            .bind(refund)
            .fetch_one(&mut *tx)
            .await?;
            let Some((card,)) = card.filter(|_| !done.0) else {
                return Ok(());
            };
            sqlx::query("SELECT 1 FROM gift_cards WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
                .bind(&event.tenant_id)
                .bind(&card)
                .execute(&mut *tx)
                .await?;
            let (card, _) = ledger(
                &mut tx,
                &scope,
                &card,
                "refunded",
                amount,
                Some(&p.reference),
                Some(refund),
                "",
            )
            .await?;
            scope
                .publish(
                    &mut tx,
                    "gift_card.refunded",
                    &json!({ "id": card.id, "order_id": p.reference, "amount_minor": amount, "balance_minor": card.balance_minor }),
                )
                .await?;
            tx.commit().await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        let c = new_code().unwrap();
        assert_eq!(c.len(), 19);
        assert!(c.bytes().all(|b| b == b'-' || ALPHABET.contains(&b)));
        assert_eq!(normalize(" abcd-efgh 2345 "), "ABCDEFGH2345");
        assert_eq!(last_chars("ABCDEFGH2345"), "2345");
        assert_ne!(new_code().unwrap(), c);
    }
}
