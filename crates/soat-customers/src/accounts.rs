//! Customer accounts: register, sign in and manage your own profile on the
//! storefront.
//!
//! These endpoints are public: the shop comes from the `Host` (or the
//! `x-soat-tenant` header), the customer from a bearer token
//! `soat_ct_...` that `login` or `register` returns. Customer sessions
//! are separate from staff sessions and only valid in their own shop.
//!
//! An email that is already a customer without an account (a guest
//! order, an import) cannot be claimed by registering: that needs a link
//! sent to the address, which comes with transactional email (C16).

use soat_auth::normalize_email;
use soat_tenancy::{Shop, TenantTx};
use zapline::auth::password::{hash, hash_token, new_token, verify, verify_missing};
use zapline::auth::BearerToken;
use zapline::db::{sqlx, Db};
use zapline::extract::FromRequestParts;
use zapline::prelude::*;

use crate::CustomerId;

/// How long a customer stays signed in.
pub const SESSION_DAYS: i64 = 30;
/// The prefix of a customer session token.
pub const TOKEN_PREFIX: &str = "soat_ct_";

/// What a customer sees of themselves.
#[model]
#[derive(sqlx::FromRow)]
pub struct Me {
    pub id: String,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub phone: String,
    pub accepts_marketing: bool,
}

/// A new customer session.
#[model]
pub struct CustomerSession {
    /// Send as `Authorization: Bearer <token>`.
    pub token: String,
    pub customer: Me,
}

/// Body of a registration.
#[model]
pub struct Register {
    #[validate(email, length(max = 254))]
    pub email: String,
    #[validate(length(min = 10, max = 200))]
    pub password: String,
    #[validate(length(max = 100))]
    pub first_name: Option<String>,
    #[validate(length(max = 100))]
    pub last_name: Option<String>,
    #[serde(default)]
    pub accepts_marketing: bool,
}

/// Body of a sign-in.
#[model]
pub struct Login {
    #[validate(email, length(max = 254))]
    pub email: String,
    #[validate(length(min = 1, max = 200))]
    pub password: String,
}

/// Body of a profile change; the email needs a confirmation flow (C16).
#[model]
pub struct MeChange {
    #[validate(length(max = 100))]
    pub first_name: Option<String>,
    #[validate(length(max = 100))]
    pub last_name: Option<String>,
    #[validate(length(max = 40))]
    pub phone: Option<String>,
    pub accepts_marketing: Option<bool>,
}

const ME: &str = "id, email, first_name, last_name, phone, accepts_marketing";

/// The signed-in customer of the request's shop. Checked when
/// [`SignedIn::begin`] opens the shop's transaction.
pub struct SignedIn {
    pub shop: Shop,
    token_hash: String,
}

impl FromRequestParts for SignedIn {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let shop = Shop::from_request_parts(req).await?;
        let bearer = BearerToken::from_request_parts(req).await?;
        if !bearer.token().starts_with(TOKEN_PREFIX) {
            return Err(Error::unauthorized().with_code("invalid_session"));
        }
        Ok(SignedIn {
            shop,
            token_hash: hash_token(bearer.token()),
        })
    }
}

impl SignedIn {
    /// The shop's transaction and the customer's id; 401 for an unknown or
    /// expired session.
    pub async fn begin(&self, db: &Db) -> Result<(TenantTx, String)> {
        let mut tx = self.shop.begin(db).await?;
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT customer_id FROM customer_sessions \
             WHERE tenant_id = $1 AND token_hash = $2 AND expires_at > now()",
        )
        .bind(self.shop.id())
        .bind(&self.token_hash)
        .fetch_optional(&mut *tx)
        .await?;
        let (customer,) = row.ok_or_else(|| Error::unauthorized().with_code("invalid_session"))?;
        Ok((tx, customer))
    }
}

pub(crate) fn mount(app: App) -> App {
    app.mount((register, login, logout, get_me, update_me))
}

async fn start_session(tx: &mut TenantTx, shop: &Shop, customer: Me) -> Result<CustomerSession> {
    let token = format!("{TOKEN_PREFIX}{}", new_token()?);
    sqlx::query(
        "INSERT INTO customer_sessions (token_hash, tenant_id, customer_id, expires_at) \
         VALUES ($1, $2, $3, now() + make_interval(days => $4))",
    )
    .bind(hash_token(&token))
    .bind(shop.id())
    .bind(&customer.id)
    .bind(SESSION_DAYS as i32)
    .execute(&mut **tx)
    .await?;
    Ok(CustomerSession { token, customer })
}

/// Create a customer account and sign in. An email with an account is a
/// 409 `email_taken`; one known without an account is a 409
/// `activation_required`. Publishes `customer.created` (or nothing new for
/// the profile) and `customer.registered`.
#[post("/api/storefront/account/register", tag = "storefront", status = 201)]
async fn register(
    db: Db,
    shop: Shop,
    Valid(Json(r)): Valid<Json<Register>>,
) -> Result<(StatusCode, Json<CustomerSession>)> {
    let email = normalize_email(&r.email);
    let password_hash = hash(&r.password).await?;
    let mut tx = shop.begin(&db).await?;
    let existing: Option<(bool,)> = sqlx::query_as(
        "SELECT password_hash IS NOT NULL FROM customers WHERE tenant_id = $1 AND email = $2",
    )
    .bind(shop.id())
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?;
    match existing {
        Some((true,)) => {
            return Err(
                Error::conflict("an account with this email exists; sign in")
                    .with_code("email_taken"),
            )
        }
        Some((false,)) => {
            return Err(Error::conflict(
                "this email is already a customer; an activation link is needed",
            )
            .with_code("activation_required"))
        }
        None => {}
    }
    let me = sqlx::query_as::<_, Me>(&format!(
        "INSERT INTO customers (id, tenant_id, email, first_name, last_name, \
         accepts_marketing, marketing_consent_at, password_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $6 THEN now() END, $7) RETURNING {ME}"
    ))
    .bind(CustomerId::new().as_str())
    .bind(shop.id())
    .bind(&email)
    .bind(r.first_name.unwrap_or_default())
    .bind(r.last_name.unwrap_or_default())
    .bind(r.accepts_marketing)
    .bind(password_hash)
    .fetch_one(&mut *tx)
    .await?;
    let actor = Some(me.id.as_str());
    shop.publish(&mut tx, actor, "customer.created", &me)
        .await?;
    shop.publish(
        &mut tx,
        actor,
        "customer.registered",
        &serde_json::json!({ "id": me.id }),
    )
    .await?;
    let session = start_session(&mut tx, &shop, me).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(session)))
}

/// Sign in with email and password. Wrong credentials are a 401
/// `invalid_credentials`, the same whether or not the email exists.
#[post("/api/storefront/account/login", tag = "storefront")]
async fn login(
    db: Db,
    shop: Shop,
    Valid(Json(l)): Valid<Json<Login>>,
) -> Result<Json<CustomerSession>> {
    let mut tx = shop.begin(&db).await?;
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT id, password_hash FROM customers WHERE tenant_id = $1 AND email = $2",
    )
    .bind(shop.id())
    .bind(normalize_email(&l.email))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, Some(password_hash))) = row else {
        // Same work as a real check, so timing does not reveal the email.
        verify_missing(l.password).await?;
        return Err(Error::unauthorized().with_code("invalid_credentials"));
    };
    if !verify(l.password, password_hash).await? {
        return Err(Error::unauthorized().with_code("invalid_credentials"));
    }
    let me = sqlx::query_as::<_, Me>(&format!(
        "SELECT {ME} FROM customers WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(shop.id())
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;
    let session = start_session(&mut tx, &shop, me).await?;
    tx.commit().await?;
    Ok(Json(session))
}

/// Sign out: the token stops working.
#[post("/api/storefront/account/logout", tag = "storefront", status = 204)]
async fn logout(db: Db, signed_in: SignedIn) -> Result<StatusCode> {
    let (mut tx, _) = signed_in.begin(&db).await?;
    sqlx::query("DELETE FROM customer_sessions WHERE tenant_id = $1 AND token_hash = $2")
        .bind(signed_in.shop.id())
        .bind(&signed_in.token_hash)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The signed-in customer's profile.
#[get("/api/storefront/account", tag = "storefront")]
async fn get_me(db: Db, signed_in: SignedIn) -> Result<Json<Me>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let me = sqlx::query_as::<_, Me>(&format!(
        "SELECT {ME} FROM customers WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(signed_in.shop.id())
    .bind(&customer)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(me))
}

/// Change your own name, phone or marketing consent. Publishes
/// `customer.updated`.
#[patch("/api/storefront/account", tag = "storefront")]
async fn update_me(
    db: Db,
    signed_in: SignedIn,
    Valid(Json(c)): Valid<Json<MeChange>>,
) -> Result<Json<Me>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let me = sqlx::query_as::<_, Me>(&format!(
        "UPDATE customers SET first_name = COALESCE($3, first_name), \
         last_name = COALESCE($4, last_name), phone = COALESCE($5, phone), \
         marketing_consent_at = CASE WHEN $6 IS NULL THEN marketing_consent_at \
             WHEN $6 THEN COALESCE(marketing_consent_at, now()) END, \
         accepts_marketing = COALESCE($6, accepts_marketing) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {ME}"
    ))
    .bind(signed_in.shop.id())
    .bind(&customer)
    .bind(&c.first_name)
    .bind(&c.last_name)
    .bind(&c.phone)
    .bind(c.accepts_marketing)
    .fetch_one(&mut *tx)
    .await?;
    signed_in
        .shop
        .publish(&mut tx, Some(&customer), "customer.updated", &me)
        .await?;
    tx.commit().await?;
    Ok(Json(me))
}
