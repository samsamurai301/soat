//! The shopper's side: create a cart, hold it by its token
//! (`x-soat-cart-token`), change its lines, attach it to a signed-in
//! customer.

use soat_catalog::stock_item;
use soat_core::Currency;
use soat_customers::accounts::SignedIn;
use soat_inventory::reservations::availability;
use soat_tenancy::{Acting, Scope, Shop, TenantTx};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::{sqlx, Db};
use zapline::extract::FromRequestParts;
use zapline::prelude::*;

use crate::{view, CartId, CartView, LineId, MAX_LINES, MAX_QUANTITY};

/// The header holding a cart's token.
pub const TOKEN_HEADER: &str = "x-soat-cart-token";

/// A new cart and the token that holds it.
#[model]
pub struct NewCartResponse {
    /// Send as the `x-soat-cart-token` header; shown once.
    pub token: String,
    pub cart: CartView,
}

/// Body of a cart creation.
#[model]
pub struct NewCart {
    /// ISO 4217, e.g. `EUR`.
    pub currency: String,
    #[validate(email, length(max = 254))]
    pub email: Option<String>,
}

/// Body of a cart change.
#[model]
pub struct CartChange {
    #[validate(email, length(max = 254))]
    pub email: Option<String>,
}

/// Body of adding an item.
#[model]
pub struct AddLine {
    /// A variant, or a product without variants.
    pub item_id: String,
    #[validate(range(min = 1, max = 999))]
    pub quantity: i64,
}

/// Body of a line change.
#[model]
pub struct LineChange {
    /// 0 removes the line.
    #[validate(range(min = 0, max = 999))]
    pub quantity: i64,
}

/// A request holding a cart: the shop and the cart's token.
pub struct CartToken {
    pub shop: Shop,
    token_hash: String,
}

impl CartToken {
    /// The holder of `token` in `shop`, e.g. from a storefront cookie.
    pub fn new(shop: Shop, token: &str) -> Self {
        Self {
            shop,
            token_hash: hash_token(token),
        }
    }
}

impl FromRequestParts for CartToken {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let shop = Shop::from_request_parts(req).await?;
        let token = req
            .headers()
            .get(TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::unauthorized().with_code("missing_cart_token"))?;
        Ok(CartToken {
            token_hash: hash_token(token),
            shop,
        })
    }
}

impl CartToken {
    /// The shop's transaction, with the open cart `id` locked; 404 for a
    /// wrong token or another shop's cart, 409 once it is completed.
    pub async fn open(&self, db: &Db, id: &str) -> Result<(TenantTx, String)> {
        let id = CartId::parse(id).map_err(|_| Error::not_found())?;
        let mut tx = self.shop.begin(db).await?;
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM carts WHERE tenant_id = $1 AND id = $2 AND token_hash = $3 \
             FOR UPDATE",
        )
        .bind(self.shop.id())
        .bind(id.as_str())
        .bind(&self.token_hash)
        .fetch_optional(&mut *tx)
        .await?;
        match row {
            None => Err(Error::not_found()),
            Some((status,)) if status != "open" => {
                Err(Error::conflict("this cart is already checked out").with_code("cart_completed"))
            }
            Some(_) => Ok((tx, id.to_string())),
        }
    }

    /// Like [`open`](Self::open), but completed carts can be read.
    async fn read(&self, db: &Db, id: &str) -> Result<(TenantTx, String)> {
        let id = CartId::parse(id).map_err(|_| Error::not_found())?;
        let mut tx = self.shop.begin(db).await?;
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM carts WHERE tenant_id = $1 AND id = $2 AND token_hash = $3",
        )
        .bind(self.shop.id())
        .bind(id.as_str())
        .bind(&self.token_hash)
        .fetch_optional(&mut *tx)
        .await?;
        row.map(|(id,)| (tx, id)).ok_or_else(Error::not_found)
    }
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        create_cart,
        read_cart,
        update_cart,
        add_line,
        change_line,
        remove_line,
        attach_customer,
    ))
}

async fn touch_and_view(tx: &mut TenantTx, scope: &impl Scope, id: &str) -> Result<CartView> {
    sqlx::query("UPDATE carts SET updated_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(scope.tenant_id())
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let cart = view(tx, scope, id).await?;
    scope.publish(tx, "cart.updated", &cart).await?;
    Ok(cart)
}

/// Starts a cart in `currency` and returns its token (shown once) and the
/// cart. Publishes `cart.created`.
pub async fn create(
    tx: &mut TenantTx,
    shop: &Shop,
    currency: &str,
    email: Option<&str>,
) -> Result<(String, CartView)> {
    let currency = Currency::parse(currency)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))?;
    let token = format!("soat_cart_{}", new_token()?);
    let id = CartId::new();
    sqlx::query(
        "INSERT INTO carts (id, tenant_id, token_hash, currency, email) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id.as_str())
    .bind(shop.id())
    .bind(hash_token(&token))
    .bind(currency.as_str())
    .bind(email.unwrap_or_default().trim().to_lowercase())
    .execute(&mut **tx)
    .await?;
    let cart = view(tx, shop, id.as_str()).await?;
    shop.publish(tx, None, "cart.created", &cart).await?;
    Ok((token, cart))
}

/// Start a cart in a currency. Returns its token once: keep it to change
/// the cart. Publishes `cart.created`.
#[post("/api/storefront/carts", tag = "storefront", status = 201)]
async fn create_cart(
    db: Db,
    shop: Shop,
    Valid(Json(c)): Valid<Json<NewCart>>,
) -> Result<(StatusCode, Json<NewCartResponse>)> {
    let mut tx = shop.begin(&db).await?;
    let (token, cart) = create(&mut tx, &shop, &c.currency, c.email.as_deref()).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(NewCartResponse { token, cart })))
}

/// The cart with today's prices and stock.
#[get("/api/storefront/carts/{id}", tag = "storefront")]
async fn read_cart(db: Db, holder: CartToken, Path(id): Path<String>) -> Result<Json<CartView>> {
    let (mut tx, id) = holder.read(&db, &id).await?;
    let cart = view(&mut tx, &holder.shop, &id).await?;
    tx.commit().await?;
    Ok(Json(cart))
}

/// Set the cart's email. Publishes `cart.updated`.
#[patch("/api/storefront/carts/{id}", tag = "storefront")]
async fn update_cart(
    db: Db,
    holder: CartToken,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CartChange>>,
) -> Result<Json<CartView>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    if let Some(email) = &c.email {
        sqlx::query("UPDATE carts SET email = $3 WHERE tenant_id = $1 AND id = $2")
            .bind(holder.shop.id())
            .bind(&id)
            .bind(email.trim().to_lowercase())
            .execute(&mut *tx)
            .await?;
    }
    let cart = touch_and_view(&mut tx, &holder.shop, &id).await?;
    tx.commit().await?;
    Ok(Json(cart))
}

/// Adds units of an item to open cart `id` (locked with
/// [`CartToken::open`]). Publishes `cart.updated`.
///
/// # Errors
///
/// 422 for an unknown item or one not sold in the cart's currency, 409
/// `insufficient_stock` for more than can be sold.
pub async fn add(
    tx: &mut TenantTx,
    shop: &Shop,
    id: &str,
    item_id: &str,
    quantity_added: i64,
) -> Result<CartView> {
    let item = stock_item(&mut *tx, shop, item_id).await?.ok_or_else(|| {
        Error::unprocessable(format!("no item {}", item_id)).with_code("unknown_item")
    })?;
    let (currency, current, lines): (String, Option<i32>, i64) = sqlx::query_as(
        "SELECT c.currency, \
         (SELECT quantity FROM cart_lines WHERE cart_id = c.id AND item_id = $3), \
         (SELECT COUNT(*) FROM cart_lines WHERE cart_id = c.id) \
         FROM carts c WHERE c.tenant_id = $1 AND c.id = $2",
    )
    .bind(shop.id())
    .bind(id)
    .bind(&item.id)
    .fetch_one(&mut **tx)
    .await?;
    let quantity = i64::from(current.unwrap_or(0)) + quantity_added;
    if current.is_none() && lines >= MAX_LINES {
        return Err(
            Error::conflict(format!("a cart holds at most {MAX_LINES} lines"))
                .with_code("too_many_lines"),
        );
    }
    if quantity > MAX_QUANTITY {
        return Err(
            Error::unprocessable(format!("at most {MAX_QUANTITY} of one item"))
                .with_code("invalid_quantity"),
        );
    }
    let code = Currency::parse(&currency).map_err(|e| Error::internal(e.to_string()))?;
    if soat_pricing::resolve(&mut *tx, shop, &item.id, code)
        .await?
        .is_none()
    {
        return Err(
            Error::unprocessable(format!("{} is not sold in {currency}", item.sku))
                .with_code("not_sold_in_currency"),
        );
    }
    let now = availability(&mut *tx, shop, &item.id).await?;
    if !now.can_sell(quantity) {
        return Err(
            Error::conflict(format!("only {} available", now.available()))
                .with_code("insufficient_stock"),
        );
    }
    sqlx::query(
        "INSERT INTO cart_lines (id, tenant_id, cart_id, item_id, quantity) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (cart_id, item_id) DO UPDATE SET quantity = EXCLUDED.quantity",
    )
    .bind(LineId::new().as_str())
    .bind(shop.id())
    .bind(id)
    .bind(&item.id)
    .bind(quantity as i32)
    .execute(&mut **tx)
    .await?;
    touch_and_view(tx, shop, id).await
}

/// Add units of an item; adding an item already in the cart raises its
/// quantity. An unknown item, or one not sold in the cart's currency, is a
/// 422; more than can be sold is a 409 `insufficient_stock`. Publishes
/// `cart.updated`.
#[post("/api/storefront/carts/{id}/lines", tag = "storefront")]
async fn add_line(
    db: Db,
    holder: CartToken,
    Path(id): Path<String>,
    Valid(Json(a)): Valid<Json<AddLine>>,
) -> Result<Json<CartView>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let cart = add(&mut tx, &holder.shop, &id, &a.item_id, a.quantity).await?;
    tx.commit().await?;
    Ok(Json(cart))
}

/// Sets the quantity of line `line_id` of open cart `id`; 0 removes it.
/// Publishes `cart.updated`.
///
/// # Errors
///
/// 404 for another cart's line, 409 `insufficient_stock` for more than can
/// be sold.
pub async fn set_quantity(
    tx: &mut TenantTx,
    shop: &Shop,
    id: &str,
    line_id: &str,
    quantity: i64,
) -> Result<CartView> {
    let item: Option<(String,)> = sqlx::query_as(
        "SELECT item_id FROM cart_lines WHERE tenant_id = $1 AND cart_id = $2 AND id = $3",
    )
    .bind(shop.id())
    .bind(id)
    .bind(line_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (item_id,) = item.ok_or_else(Error::not_found)?;
    if quantity == 0 {
        sqlx::query("DELETE FROM cart_lines WHERE tenant_id = $1 AND id = $2")
            .bind(shop.id())
            .bind(line_id)
            .execute(&mut **tx)
            .await?;
    } else {
        let now = availability(&mut *tx, shop, &item_id).await?;
        if !now.can_sell(quantity) {
            return Err(
                Error::conflict(format!("only {} available", now.available()))
                    .with_code("insufficient_stock"),
            );
        }
        sqlx::query("UPDATE cart_lines SET quantity = $3 WHERE tenant_id = $1 AND id = $2")
            .bind(shop.id())
            .bind(line_id)
            .bind(quantity as i32)
            .execute(&mut **tx)
            .await?;
    }
    touch_and_view(tx, shop, id).await
}

/// Set a line's quantity; 0 removes it. More than can be sold is a 409
/// `insufficient_stock`. Publishes `cart.updated`.
#[patch("/api/storefront/carts/{id}/lines/{line_id}", tag = "storefront")]
async fn change_line(
    db: Db,
    holder: CartToken,
    Path((id, line_id)): Path<(String, String)>,
    Valid(Json(c)): Valid<Json<LineChange>>,
) -> Result<Json<CartView>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let cart = set_quantity(&mut tx, &holder.shop, &id, &line_id, c.quantity).await?;
    tx.commit().await?;
    Ok(Json(cart))
}

/// Remove a line. Publishes `cart.updated`.
#[delete("/api/storefront/carts/{id}/lines/{line_id}", tag = "storefront")]
async fn remove_line(
    db: Db,
    holder: CartToken,
    Path((id, line_id)): Path<(String, String)>,
) -> Result<Json<CartView>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let gone =
        sqlx::query("DELETE FROM cart_lines WHERE tenant_id = $1 AND cart_id = $2 AND id = $3")
            .bind(holder.shop.id())
            .bind(&id)
            .bind(&line_id)
            .execute(&mut *tx)
            .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    let cart = touch_and_view(&mut tx, &holder.shop, &id).await?;
    tx.commit().await?;
    Ok(Json(cart))
}

/// Attach the cart to the signed-in customer (customer bearer token plus
/// the cart token); their email becomes the cart's. Publishes
/// `cart.updated`.
#[post("/api/storefront/carts/{id}/customer", tag = "storefront")]
async fn attach_customer(
    db: Db,
    holder: CartToken,
    signed_in: SignedIn,
    Path(id): Path<String>,
) -> Result<Json<CartView>> {
    let (tx, customer) = signed_in.begin(&db).await?;
    tx.commit().await?;
    let (mut tx, id) = holder.open(&db, &id).await?;
    let cart = attach(&mut tx, &holder.shop, &id, &customer).await?;
    tx.commit().await?;
    Ok(Json(cart))
}

/// Gives cart `id` to customer `customer`, whose email becomes the
/// cart's. Run it in a transaction from [`CartToken::open`]. Publishes
/// `cart.updated` with the customer as the actor.
///
/// # Errors
///
/// Database errors.
pub async fn attach(tx: &mut TenantTx, shop: &Shop, id: &str, customer: &str) -> Result<CartView> {
    sqlx::query(
        "UPDATE carts SET customer_id = $3, \
         email = (SELECT email FROM customers WHERE tenant_id = $1 AND id = $3) \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(shop.id())
    .bind(id)
    .bind(customer)
    .execute(&mut **tx)
    .await?;
    let acting = Acting {
        shop,
        actor: Some(customer.to_owned()),
    };
    touch_and_view(tx, &acting, id).await
}

/// The open cart `id` if `token` holds it, read in the shop's transaction.
pub async fn held(
    conn: &mut zapline::db::sqlx::PgConnection,
    shop: &Shop,
    id: &str,
    token: &str,
) -> Result<Option<CartView>> {
    let Ok(id) = CartId::parse(id) else {
        return Ok(None);
    };
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM carts WHERE tenant_id = $1 AND id = $2 AND token_hash = $3 \
         AND status = 'open'",
    )
    .bind(shop.id())
    .bind(id.as_str())
    .bind(hash_token(token))
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        Some((id,)) => Ok(Some(view(conn, shop, &id).await?)),
        None => Ok(None),
    }
}
