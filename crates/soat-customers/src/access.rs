//! Access links: a customer who forgot their password, or whose email
//! ordered here as a guest and never had an account, asks for a link by
//! email and sets a password with it.
//!
//! Asking always answers the same, so it never tells whether an email is
//! a customer. A link works once, for an hour; using it ends the
//! customer's other sessions. The token never goes into an event: the
//! event names the link, and the email module reads the token once to
//! write the email ([`access_mail`]), which clears it.

use serde_json::json;
use soat_auth::normalize_email;
use soat_core::id_kind;
use soat_tenancy::{Scope, Shop};
use zapline::auth::password::{hash, hash_token, new_token};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::accounts::{self, CustomerSession};

id_kind!(AccessLinkId, AccessLinkKind, "cal");

/// How long a link works.
pub const LINK_MINUTES: i32 = 60;
/// Most links one customer may ask for in an hour.
const LINKS_PER_HOUR: i64 = 3;

/// Body of a request for a link.
#[model]
pub struct AskForAccess {
    #[validate(email, length(max = 254))]
    pub email: String,
}

/// The answer to a request for a link, the same for every email.
#[model]
pub struct AccessAsked {
    /// Always true: an email is on its way if the address is a customer's.
    pub sent: bool,
}

/// Body of setting a password with a link.
#[model]
pub struct UseAccess {
    #[validate(length(min = 1, max = 200))]
    pub token: String,
    #[validate(length(min = 10, max = 200))]
    pub password: String,
}

/// What the email module needs to write the email.
#[derive(Debug, Clone)]
pub struct AccessMail {
    pub email: String,
    pub first_name: String,
    /// `reset` or `activate`.
    pub purpose: String,
    pub token: String,
    /// The page to set a password at, when the shop has a domain.
    pub link: Option<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((ask_for_access, use_access))
}

/// The shop's address for the link: the request's host when it is one of
/// the shop's domains, else its first domain; none without a domain.
async fn shop_domain(
    conn: &mut PgConnection,
    shop: &Shop,
    host: Option<&str>,
) -> Result<Option<String>> {
    let domains: Vec<(String,)> =
        sqlx::query_as("SELECT domain FROM tenant_domains WHERE tenant_id = $1 ORDER BY domain")
            .bind(shop.id())
            .fetch_all(conn)
            .await?;
    let host = host.map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase());
    Ok(domains
        .iter()
        .find(|(d,)| Some(d) == host.as_ref())
        .or(domains.first())
        .map(|(d,)| d.clone()))
}

/// Sends a link to set a password, if `email` is a customer of the shop.
/// Publishes `customer.access_requested` (the link's id, never its token).
///
/// # Errors
///
/// Database errors.
pub async fn ask(db: &Db, shop: &Shop, email: &str, host: Option<&str>) -> Result<()> {
    let mut tx = shop.begin(db).await?;
    let found: Option<(String, bool)> = sqlx::query_as(
        "SELECT id, password_hash IS NOT NULL FROM customers WHERE tenant_id = $1 AND email = $2",
    )
    .bind(shop.id())
    .bind(normalize_email(email))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((customer, has_password)) = found else {
        tx.commit().await?;
        return Ok(());
    };
    let (recent,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM customer_access_links WHERE tenant_id = $1 \
         AND customer_id = $2 AND created_at > now() - interval '1 hour'",
    )
    .bind(shop.id())
    .bind(&customer)
    .fetch_one(&mut *tx)
    .await?;
    if recent >= LINKS_PER_HOUR {
        tx.commit().await?;
        return Ok(());
    }
    let token = new_token()?;
    let link = shop_domain(&mut tx, shop, host)
        .await?
        .map(|d| format!("https://{d}/account/access?token={token}"));
    let purpose = if has_password { "reset" } else { "activate" };
    let id = AccessLinkId::new();
    sqlx::query(
        "INSERT INTO customer_access_links (id, tenant_id, customer_id, purpose, token_hash, \
         token, link, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(mins => $8))",
    )
    .bind(id.as_str())
    .bind(shop.id())
    .bind(&customer)
    .bind(purpose)
    .bind(hash_token(&token))
    .bind(&token)
    .bind(&link)
    .bind(LINK_MINUTES)
    .execute(&mut *tx)
    .await?;
    shop.publish(
        &mut tx,
        None,
        "customer.access_requested",
        &json!({ "id": id.as_str(), "customer_id": customer, "purpose": purpose }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Email, first name, purpose, token and link of a link to write.
type MailRow = (String, String, String, Option<String>, Option<String>);

/// The email for link `id`, once: the token is cleared as it is read.
/// None for a link already written, used or expired.
///
/// # Errors
///
/// Database errors.
pub async fn access_mail(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
) -> Result<Option<AccessMail>> {
    let row: Option<MailRow> = sqlx::query_as(
        "WITH old AS (SELECT l.id, l.token, l.link, l.purpose, c.email, c.first_name \
         FROM customer_access_links l JOIN customers c \
         ON c.tenant_id = l.tenant_id AND c.id = l.customer_id \
         WHERE l.tenant_id = $1 AND l.id = $2 AND l.token IS NOT NULL \
         AND l.used_at IS NULL AND l.expires_at > now() FOR UPDATE OF l) \
         UPDATE customer_access_links SET token = NULL, link = NULL FROM old \
         WHERE customer_access_links.id = old.id \
         RETURNING old.email, old.first_name, old.purpose, old.token, old.link",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(row.and_then(|(email, first_name, purpose, token, link)| {
        Some(AccessMail {
            email,
            first_name,
            purpose,
            token: token?,
            link,
        })
    }))
}

/// Sets the customer's password with a link and signs them in, ending
/// their other sessions. Publishes `customer.password_set`, and
/// `customer.registered` when it activates an account.
///
/// # Errors
///
/// 422 `invalid_link` for a link that is unknown, used or expired;
/// database errors.
pub async fn use_link(db: &Db, shop: &Shop, u: UseAccess) -> Result<CustomerSession> {
    let password_hash = hash(&u.password).await?;
    let mut tx = shop.begin(db).await?;
    let found: Option<(String, String)> = sqlx::query_as(
        "UPDATE customer_access_links SET used_at = now(), token = NULL, link = NULL \
         WHERE tenant_id = $1 AND token_hash = $2 AND used_at IS NULL AND expires_at > now() \
         RETURNING customer_id, purpose",
    )
    .bind(shop.id())
    .bind(hash_token(u.token.trim()))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((customer, purpose)) = found else {
        return Err(
            Error::unprocessable("this link does not work any more; ask for a new one")
                .with_code("invalid_link"),
        );
    };
    sqlx::query("UPDATE customers SET password_hash = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(shop.id())
        .bind(&customer)
        .bind(password_hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM customer_sessions WHERE tenant_id = $1 AND customer_id = $2")
        .bind(shop.id())
        .bind(&customer)
        .execute(&mut *tx)
        .await?;
    // Other links for the customer stop working too.
    sqlx::query(
        "UPDATE customer_access_links SET used_at = now(), token = NULL, link = NULL \
         WHERE tenant_id = $1 AND customer_id = $2 AND used_at IS NULL",
    )
    .bind(shop.id())
    .bind(&customer)
    .execute(&mut *tx)
    .await?;
    let actor = Some(customer.as_str());
    let payload = json!({ "id": customer });
    shop.publish(&mut tx, actor, "customer.password_set", &payload)
        .await?;
    if purpose == "activate" {
        shop.publish(&mut tx, actor, "customer.registered", &payload)
            .await?;
    }
    let me = accounts::profile(&mut tx, shop, &customer).await?;
    let session = accounts::start_session(&mut tx, shop, me).await?;
    tx.commit().await?;
    Ok(session)
}

/// Ask for a link to set a password: for a forgotten one, or to activate
/// an email that ordered as a guest. The answer is the same whether or not
/// the email is a customer's; at most three links an hour are sent.
#[post("/api/storefront/account/access", tag = "storefront")]
async fn ask_for_access(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Valid(Json(a)): Valid<Json<AskForAccess>>,
) -> Result<Json<AccessAsked>> {
    let host = headers.get("host").and_then(|h| h.to_str().ok());
    ask(&db, &shop, &a.email, host).await?;
    Ok(Json(AccessAsked { sent: true }))
}

/// Set a password with the token from a link, and sign in. A link works
/// once, for an hour: otherwise a 422 `invalid_link`.
#[post("/api/storefront/account/access/complete", tag = "storefront")]
async fn use_access(
    db: Db,
    shop: Shop,
    Valid(Json(u)): Valid<Json<UseAccess>>,
) -> Result<Json<CustomerSession>> {
    Ok(Json(use_link(&db, &shop, u).await?))
}
