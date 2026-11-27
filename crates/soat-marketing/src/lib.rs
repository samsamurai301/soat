//! Email campaigns.
//!
//! Staff write a campaign (subject, text, optional HTML, in the small
//! template language of [`soat_core::template`] with the customer's
//! `first_name`, `last_name`, `email`, the `shop` and the
//! `unsubscribe_url`), send themselves a test, then send it. Sending fixes
//! who gets it: the customers who agreed to news (optionally only those
//! with a tag). A scheduled job ([`send_due`]) then mails them in
//! batches. Every email carries a one-click unsubscribe link (also as a
//! `List-Unsubscribe` header); following it ([`unsubscribe`]) stops the
//! customer's news. A shop needs a domain to send campaigns, for that
//! link. The link's token is in no event and not kept.

use std::time::Duration;

use serde_json::{json, Value};
use soat_core::template::Template;
use soat_core::{id_kind, Permission};
use soat_module::Module;
use soat_notify::MailFrom;
use soat_tenancy::{EventShop, Scope, Shop, Tenant, TenantTx};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::mail::{Email, Mailer};
use zapline::prelude::*;
use zapline::Resources;

id_kind!(CampaignId, CampaignKind, "cmp");

/// See email campaigns and how they went.
pub const MARKETING_READ: Permission =
    Permission::new("marketing:read", "See email campaigns and their results").for_staff();
/// Write and send email campaigns.
pub const MARKETING_MANAGE: Permission =
    Permission::new("marketing:manage", "Write and send email campaigns").for_staff();

/// How often the app sends waiting campaign emails.
pub const SEND_EVERY: Duration = Duration::from_secs(30);
/// Most emails of one campaign sent in one go.
const BATCH: i64 = 200;

/// The marketing module.
pub struct Marketing;

impl Module for Marketing {
    fn name(&self) -> &'static str {
        "marketing"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "customers", "notify"]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[MARKETING_READ, MARKETING_MANAGE]
    }

    fn personal_data(&self) -> &'static [soat_module::PersonalData] {
        &[soat_module::PersonalData {
            table: "campaign_recipients",
            matches: "customer_id = $2 OR lower(email) = $3",
            export: &[
                "campaign_id",
                "email",
                "status",
                "sent_at",
                "unsubscribed_at",
            ],
            erase: soat_module::Erase::Delete,
        }]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_campaigns,
            create_campaign,
            get_campaign,
            update_campaign,
            delete_campaign,
            test_campaign,
            send_campaign,
            cancel_campaign,
            unsubscribe_api,
        ))
        .schedule(
            zapline::tasks::Schedule::every(SEND_EVERY).name("soat-campaigns"),
            |res: Resources| async move { send_due(&res).await.map(|_| ()) },
        )
    }
}

/// An email campaign and how it went.
#[model]
#[derive(sqlx::FromRow)]
pub struct Campaign {
    /// e.g. `cmp_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// For staff only, e.g. `Winter sale`.
    pub name: String,
    pub subject: String,
    pub text_body: String,
    /// Empty sends text only.
    pub html_body: String,
    /// Only customers with this tag; null for everyone who agreed to news.
    pub audience_tag: Option<String>,
    /// `draft`, `sending`, `sent` or `canceled`.
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Who it goes to (once sent; before, who it would go to now).
    pub recipients: i64,
    pub sent: i64,
    pub failed: i64,
    /// Recipients who followed its unsubscribe link.
    pub unsubscribed: i64,
}

const COLUMNS: &str =
    "c.id, c.name, c.subject, c.text_body, c.html_body, c.audience_tag, c.status, \
     to_char(c.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(c.started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS started_at, \
     to_char(c.finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS finished_at, \
     CASE WHEN c.status = 'draft' THEN (SELECT COUNT(*) FROM customers u \
         WHERE u.tenant_id = c.tenant_id AND u.accepts_marketing AND u.email <> '' \
         AND (c.audience_tag IS NULL OR c.audience_tag = ANY (u.tags))) \
       ELSE (SELECT COUNT(*) FROM campaign_recipients r WHERE r.campaign_id = c.id) END \
       AS recipients, \
     (SELECT COUNT(*) FROM campaign_recipients r WHERE r.campaign_id = c.id \
         AND r.status = 'sent') AS sent, \
     (SELECT COUNT(*) FROM campaign_recipients r WHERE r.campaign_id = c.id \
         AND r.status = 'failed') AS failed, \
     (SELECT COUNT(*) FROM campaign_recipients r WHERE r.campaign_id = c.id \
         AND r.unsubscribed_at IS NOT NULL) AS unsubscribed";

/// The shop's campaigns, newest first (up to 200).
#[model]
pub struct CampaignList {
    pub items: Vec<Campaign>,
}

/// Body of writing a campaign.
#[model]
pub struct NewCampaign {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    #[validate(length(min = 1, max = 300))]
    pub subject: String,
    #[validate(length(min = 1, max = 100_000))]
    pub text_body: String,
    #[validate(length(max = 200_000))]
    pub html_body: Option<String>,
    #[validate(length(max = 100))]
    pub audience_tag: Option<String>,
}

/// Body of changing a draft; absent fields stay as they are. An empty
/// `audience_tag` sends to everyone who agreed to news.
#[model]
pub struct CampaignChange {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    #[validate(length(min = 1, max = 300))]
    pub subject: Option<String>,
    #[validate(length(min = 1, max = 100_000))]
    pub text_body: Option<String>,
    #[validate(length(max = 200_000))]
    pub html_body: Option<String>,
    #[validate(length(max = 100))]
    pub audience_tag: Option<String>,
}

/// Body of a test send.
#[model]
pub struct TestSend {
    /// Where the test goes, e.g. your own address.
    #[validate(email, length(max = 254))]
    pub email: String,
}

/// Body of following an unsubscribe link.
#[model]
pub struct Unsubscribe {
    /// The `token` of the link in the email.
    #[validate(length(min = 1, max = 200))]
    pub token: String,
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_campaign")
}

/// Checks that a wording is a template the language understands.
fn check(field: &str, source: &str) -> Result<()> {
    Template::parse(source)
        .map(|_| ())
        .map_err(|e| invalid(format!("{field}: {e}")))
}

fn tag(t: Option<&str>) -> Option<String> {
    t.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Campaign> {
    let id = CampaignId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM email_campaigns c WHERE c.tenant_id = $1 AND c.id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's email campaigns, newest first (`marketing:read`).
#[get("/api/campaigns", tag = "marketing")]
async fn list_campaigns(db: Db, tenant: Tenant) -> Result<Json<CampaignList>> {
    let mut tx = tenant.begin_for(&db, &MARKETING_READ).await?;
    let items: Vec<Campaign> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM email_campaigns c WHERE c.tenant_id = $1 ORDER BY c.id DESC LIMIT 200"
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(CampaignList { items }))
}

/// Write a campaign, as a draft (`marketing:manage`). Publishes
/// `campaign.created`.
#[post("/api/campaigns", tag = "marketing", status = 201)]
async fn create_campaign(
    db: Db,
    tenant: Tenant,
    Valid(Json(n)): Valid<Json<NewCampaign>>,
) -> Result<(StatusCode, Json<Campaign>)> {
    check("subject", &n.subject)?;
    check("text_body", &n.text_body)?;
    let html = n.html_body.unwrap_or_default();
    check("html_body", &html)?;
    let mut tx = tenant.begin_for(&db, &MARKETING_MANAGE).await?;
    let id = CampaignId::new();
    sqlx::query(
        "INSERT INTO email_campaigns (id, tenant_id, name, subject, text_body, html_body, \
         audience_tag) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(n.name.trim())
    .bind(&n.subject)
    .bind(&n.text_body)
    .bind(&html)
    .bind(tag(n.audience_tag.as_deref()))
    .execute(&mut *tx)
    .await?;
    let c = load(&mut tx, &tenant, id.as_str()).await?;
    tenant.publish(&mut tx, "campaign.created", &c).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(c)))
}

/// A campaign and how it went (`marketing:read`).
#[get("/api/campaigns/{id}", tag = "marketing")]
async fn get_campaign(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Campaign>> {
    let mut tx = tenant.begin_for(&db, &MARKETING_READ).await?;
    let c = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(c))
}

fn not_draft() -> Error {
    Error::conflict("only a draft can be changed").with_code("campaign_not_draft")
}

/// Change a draft (`marketing:manage`). A campaign being sent or sent is a
/// 409 `campaign_not_draft`. Publishes `campaign.updated`.
#[patch("/api/campaigns/{id}", tag = "marketing")]
async fn update_campaign(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CampaignChange>>,
) -> Result<Json<Campaign>> {
    for (field, v) in [
        ("subject", &c.subject),
        ("text_body", &c.text_body),
        ("html_body", &c.html_body),
    ] {
        if let Some(v) = v {
            check(field, v)?;
        }
    }
    let mut tx = tenant.begin_for(&db, &MARKETING_MANAGE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    if current.status != "draft" {
        return Err(not_draft());
    }
    sqlx::query(
        "UPDATE email_campaigns SET name = COALESCE($3, name), subject = COALESCE($4, subject), \
         text_body = COALESCE($5, text_body), html_body = COALESCE($6, html_body), \
         audience_tag = CASE WHEN $7 THEN $8 ELSE audience_tag END \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&current.id)
    .bind(c.name.as_deref().map(str::trim))
    .bind(&c.subject)
    .bind(&c.text_body)
    .bind(&c.html_body)
    .bind(c.audience_tag.is_some())
    .bind(tag(c.audience_tag.as_deref()))
    .execute(&mut *tx)
    .await?;
    let c = load(&mut tx, &tenant, &current.id).await?;
    tenant.publish(&mut tx, "campaign.updated", &c).await?;
    tx.commit().await?;
    Ok(Json(c))
}

/// Delete a draft or a finished campaign (`marketing:manage`); one being
/// sent is a 409 (cancel it first). Publishes `campaign.deleted`.
#[delete("/api/campaigns/{id}", tag = "marketing", status = 204)]
async fn delete_campaign(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &MARKETING_MANAGE).await?;
    let c = load(&mut tx, &tenant, &id).await?;
    if c.status == "sending" {
        return Err(
            Error::conflict("the campaign is being sent; cancel it first")
                .with_code("campaign_sending"),
        );
    }
    sqlx::query("DELETE FROM email_campaigns WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&c.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(&mut tx, "campaign.deleted", &json!({ "id": c.id }))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The shop's first domain, for links in emails.
async fn domain(conn: &mut PgConnection, scope: &impl Scope) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT domain FROM tenant_domains WHERE tenant_id = $1 ORDER BY domain LIMIT 1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(d,)| d))
}

async fn shop_name(conn: &mut PgConnection, scope: &impl Scope) -> Result<String> {
    let (name,): (String,) = sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
        .bind(scope.tenant_id())
        .fetch_one(conn)
        .await?;
    Ok(name)
}

/// A campaign's email for one person, with its unsubscribe link.
fn email_for(c: &Campaign, data: &Value, link: &str) -> Email {
    let render = |s: &str, html: bool| {
        Template::parse(s)
            .map(|t| t.render(data, html))
            .unwrap_or_default()
    };
    let mut text = render(&c.text_body, false);
    if !c.text_body.contains("unsubscribe_url") {
        text.push_str(&format!("\n\n--\nUnsubscribe: {link}\n"));
    }
    let to = data["email"].as_str().unwrap_or_default();
    let mut email = Email::to(to)
        .subject(render(&c.subject, false))
        .text(text)
        .header("List-Unsubscribe", format!("<{link}>"))
        .header("List-Unsubscribe-Post", "List-Unsubscribe=One-Click");
    if !c.html_body.is_empty() {
        let mut html = render(&c.html_body, true);
        if !c.html_body.contains("unsubscribe_url") {
            html.push_str(&format!(
                "<p style=\"font-size:12px;color:#666\"><a href=\"{link}\">Unsubscribe</a></p>"
            ));
        }
        email = email.html(html);
    }
    email
}

/// Send yourself the campaign as a customer would get it
/// (`marketing:manage`); its unsubscribe link does nothing. A mail server
/// that refuses it is a 503.
#[post("/api/campaigns/{id}/test", tag = "marketing", status = 204)]
async fn test_campaign(
    db: Db,
    tenant: Tenant,
    mailer: State<Mailer>,
    from: State<MailFrom>,
    Path(id): Path<String>,
    Valid(Json(t)): Valid<Json<TestSend>>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &MARKETING_MANAGE).await?;
    let c = load(&mut tx, &tenant, &id).await?;
    let (sender, reply_to) = soat_notify::shop_sender(&mut tx, &tenant, &from.0 .0).await?;
    let shop = shop_name(&mut tx, &tenant).await?;
    let host = domain(&mut tx, &tenant)
        .await?
        .unwrap_or_else(|| "shop.example".into());
    tx.commit().await?;
    let link = format!("https://{host}/unsubscribe?token=test");
    let data = json!({ "first_name": "Ada", "last_name": "Lovelace", "email": t.email,
                       "shop": shop, "unsubscribe_url": link });
    let mut email = email_for(&c, &data, &link)
        .from(sender)
        .subject(format!("[Test] {}", render_subject(&c, &data)));
    if let Some(r) = reply_to {
        email = email.reply_to(r);
    }
    mailer.0.send(email).await.map_err(|e| {
        Error::service_unavailable(format!("the mail server refused it: {e}"))
            .with_code("mail_failed")
    })?;
    Ok(StatusCode::NO_CONTENT)
}

fn render_subject(c: &Campaign, data: &Value) -> String {
    Template::parse(&c.subject)
        .map(|t| t.render(data, false))
        .unwrap_or_default()
}

/// Send a draft (`marketing:manage`): it goes to the customers who agreed
/// to news (with its tag, if it has one) as they are now, in the
/// background. A shop without a domain is a 422 `no_domain` (every email
/// carries an unsubscribe link to it); nobody to send to is a 422
/// `no_recipients`. Publishes `campaign.sending`, and `campaign.sent`
/// once all went out.
#[post("/api/campaigns/{id}/send", tag = "marketing")]
async fn send_campaign(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Campaign>> {
    let mut tx = tenant.begin_for(&db, &MARKETING_MANAGE).await?;
    let c = load(&mut tx, &tenant, &id).await?;
    if c.status != "draft" {
        return Err(not_draft());
    }
    if domain(&mut tx, &tenant).await?.is_none() {
        return Err(Error::unprocessable(
            "add a domain to the shop first: every email links to it to unsubscribe",
        )
        .with_code("no_domain"));
    }
    let added = sqlx::query(
        "INSERT INTO campaign_recipients (tenant_id, campaign_id, customer_id, email) \
         SELECT $1, $2, u.id, u.email FROM customers u WHERE u.tenant_id = $1 \
         AND u.accepts_marketing AND u.email <> '' AND ($3::text IS NULL OR $3 = ANY (u.tags))",
    )
    .bind(tenant.id())
    .bind(&c.id)
    .bind(&c.audience_tag)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if added == 0 {
        return Err(
            Error::unprocessable("no customer agreed to news yet").with_code("no_recipients")
        );
    }
    sqlx::query(
        "UPDATE email_campaigns SET status = 'sending', started_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&c.id)
    .execute(&mut *tx)
    .await?;
    let c = load(&mut tx, &tenant, &c.id).await?;
    tenant.publish(&mut tx, "campaign.sending", &c).await?;
    tx.commit().await?;
    Ok(Json(c))
}

/// Stop a campaign being sent (`marketing:manage`); who got it keeps it.
/// Publishes `campaign.canceled`.
#[post("/api/campaigns/{id}/cancel", tag = "marketing")]
async fn cancel_campaign(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Campaign>> {
    let mut tx = tenant.begin_for(&db, &MARKETING_MANAGE).await?;
    let c = load(&mut tx, &tenant, &id).await?;
    if c.status != "sending" {
        return Err(
            Error::conflict("the campaign is not being sent").with_code("campaign_not_sending")
        );
    }
    sqlx::query(
        "UPDATE email_campaigns SET status = 'canceled', finished_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&c.id)
    .execute(&mut *tx)
    .await?;
    let c = load(&mut tx, &tenant, &c.id).await?;
    tenant.publish(&mut tx, "campaign.canceled", &c).await?;
    tx.commit().await?;
    Ok(Json(c))
}

/// Stop news to whoever an unsubscribe link `token` was sent to. Answers
/// whether it changed anything (following a link twice is fine).
///
/// # Errors
///
/// 404 `invalid_link` for an unknown token; database errors.
pub async fn unsubscribe(tx: &mut TenantTx, shop: &Shop, token: &str) -> Result<bool> {
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE campaign_recipients SET unsubscribed_at = COALESCE(unsubscribed_at, now()) \
         WHERE tenant_id = $1 AND unsubscribe_hash = $2 RETURNING campaign_id, customer_id",
    )
    .bind(shop.id())
    .bind(hash_token(token.trim()))
    .fetch_optional(&mut **tx)
    .await?;
    let Some((_, customer)) = row else {
        return Err(Error::new(
            StatusCode::NOT_FOUND,
            "this link does not unsubscribe anyone",
        )
        .with_code("invalid_link"));
    };
    soat_customers::unsubscribe(tx, shop, &customer).await
}

/// Follow an unsubscribe link from a campaign email: the customer gets no
/// more news. Following it again is fine.
#[post("/api/storefront/unsubscribe", tag = "storefront", status = 204)]
async fn unsubscribe_api(
    db: Db,
    shop: Shop,
    Valid(Json(u)): Valid<Json<Unsubscribe>>,
) -> Result<StatusCode> {
    let mut tx = shop.begin(&db).await?;
    unsubscribe(&mut tx, &shop, &u.token).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sends the waiting emails of every campaign being sent, a batch per
/// campaign; how many went out. Campaigns with none left are done.
///
/// # Errors
///
/// Database errors.
pub async fn send_due(res: &Resources) -> Result<usize> {
    let pool: PgPool = res.require()?;
    let mailer: Mailer = res.require()?;
    let MailFrom(address) = res.require()?;
    let shops: Vec<(String,)> = sqlx::query_as("SELECT id FROM tenants ORDER BY id")
        .fetch_all(&pool)
        .await?;
    let mut sent = 0;
    for (shop,) in shops {
        let scope = EventShop(shop);
        let mut tx = scope.begin_on(&pool).await?;
        let sending: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM email_campaigns WHERE tenant_id = $1 AND status = 'sending' ORDER BY id",
        )
        .bind(scope.tenant_id())
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        for (id,) in sending {
            sent += send_batch(&pool, &mailer, &address, &scope, &id).await?;
        }
    }
    Ok(sent)
}

async fn send_batch(
    pool: &PgPool,
    mailer: &Mailer,
    address: &str,
    scope: &EventShop,
    id: &str,
) -> Result<usize> {
    let mut tx = scope.begin_on(pool).await?;
    let c = load(&mut tx, scope, id).await?;
    if c.status != "sending" {
        return Ok(0);
    }
    let (sender, reply_to) = soat_notify::shop_sender(&mut tx, scope, address).await?;
    let shop = shop_name(&mut tx, scope).await?;
    let Some(host) = domain(&mut tx, scope).await? else {
        return Ok(0);
    };
    let batch: Vec<(String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT r.customer_id, r.email, COALESCE(u.first_name, ''), COALESCE(u.last_name, '') \
         FROM campaign_recipients r JOIN customers u ON u.id = r.customer_id \
         WHERE r.tenant_id = $1 AND r.campaign_id = $2 AND r.status = 'pending' \
         ORDER BY r.customer_id LIMIT {BATCH} FOR UPDATE OF r SKIP LOCKED"
    ))
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut sent = 0;
    for (customer, email, first, last) in &batch {
        let token = new_token()?;
        let link = format!("https://{host}/unsubscribe?token={token}");
        let data = json!({ "first_name": first, "last_name": last, "email": email,
                           "shop": shop, "unsubscribe_url": link });
        let mut mail = email_for(&c, &data, &link).from(sender.clone());
        if let Some(r) = &reply_to {
            mail = mail.reply_to(r.clone());
        }
        let outcome = mailer.send(mail).await.map_err(|e| e.to_string());
        let (status, error) = match &outcome {
            Ok(()) => ("sent", None),
            Err(e) => ("failed", Some(e.as_str())),
        };
        sent += usize::from(outcome.is_ok());
        sqlx::query(
            "UPDATE campaign_recipients SET status = $4, error = $5, unsubscribe_hash = $6, \
             sent_at = now() WHERE tenant_id = $1 AND campaign_id = $2 AND customer_id = $3",
        )
        .bind(scope.tenant_id())
        .bind(id)
        .bind(customer)
        .bind(status)
        .bind(error)
        .bind(hash_token(&token))
        .execute(&mut *tx)
        .await?;
    }
    let (left,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM campaign_recipients WHERE tenant_id = $1 \
         AND campaign_id = $2 AND status = 'pending')",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !left {
        sqlx::query(
            "UPDATE email_campaigns SET status = 'sent', finished_at = now() \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(scope.tenant_id())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let done = load(&mut tx, scope, id).await?;
        scope.publish(&mut tx, "campaign.sent", &done).await?;
    }
    tx.commit().await?;
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn campaign(text: &str, html: &str) -> Campaign {
        Campaign {
            id: "cmp_1".into(),
            name: "Winter".into(),
            subject: "Hi {{ first_name }}".into(),
            text_body: text.into(),
            html_body: html.into(),
            audience_tag: None,
            status: "draft".into(),
            created_at: String::new(),
            started_at: None,
            finished_at: None,
            recipients: 0,
            sent: 0,
            failed: 0,
            unsubscribed: 0,
        }
    }

    #[test]
    fn every_email_can_unsubscribe() {
        let data = json!({ "first_name": "Ada", "email": "ada@example.com" });
        let m = email_for(
            &campaign("Sale, {{ first_name }}!", ""),
            &data,
            "https://s/u?t=1",
        );
        let mime = String::from_utf8(m.to_mime("x@example.com").unwrap()).unwrap();
        assert!(mime.contains("Sale, Ada!"), "{mime}");
        assert!(mime.contains("Unsubscribe: https://s/u?t=1"), "{mime}");
        assert!(
            mime.contains("List-Unsubscribe: <https://s/u?t=1>"),
            "{mime}"
        );
        assert!(mime.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"));
        // A wording that places the link itself gets no second one.
        let own = campaign("Bye: {{ unsubscribe_url }}", "<p>{{ unsubscribe_url }}</p>");
        let data = json!({ "email": "a@example.com", "unsubscribe_url": "https://s/u?t=2" });
        let mime = String::from_utf8(
            email_for(&own, &data, "https://s/u?t=2")
                .to_mime("x@example.com")
                .unwrap(),
        )
        .unwrap();
        assert!(!mime.contains("Unsubscribe: https"), "{mime}");
    }
}
