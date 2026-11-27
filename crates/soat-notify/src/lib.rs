//! Notifications: transactional email to buyers and staff.
//!
//! The module listens to order events and sends an email for each:
//! confirmation when an order is placed (and a notice to the shop's staff
//! address), shipping confirmation, cancellation, refunds, and payment
//! received (off by default); and to customers, the link to reset a
//! password or activate an account (`customer.access_requested`), whose
//! body is not kept once sent. Each shop can reword any template (subject,
//! text and HTML, in the small language of [`soat_core::template`]), turn
//! templates off, and set the sender name and reply-to address. Every
//! email is logged, can be read back and resent.
//!
//! Mail goes out through Zapline's [`Mailer`]: SMTP from `SMTP_URL`, from
//! the platform's address (`MAIL_FROM`) with the shop's name. Events are
//! handled at least once; one log row per event and template keeps a
//! retried event from sending twice.

mod context;
mod defaults;

use std::sync::Arc;

use serde_json::{json, Value};
use soat_core::template::Template;
use soat_core::{id_kind, Permission};
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_orders::Order;
use soat_tenancy::{Scope, Tenant, TenantListing};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::mail::{Email, Mailer};
use zapline::prelude::*;
use zapline::Resources;

pub use defaults::{Builtin, BUILTINS};

id_kind!(EmailMessageId, EmailMessage, "eml");

/// See email settings, templates and the sent log.
pub const NOTIFICATIONS_READ: Permission = Permission::new(
    "notifications:read",
    "See email settings, templates and sent emails",
)
.for_staff();
/// Change email settings and templates, resend emails.
pub const NOTIFICATIONS_MANAGE: Permission = Permission::new(
    "notifications:manage",
    "Change email settings and templates, resend emails",
)
.for_staff();

/// The platform's sending address, `MAIL_FROM`; in the app's state and
/// resources.
#[derive(Clone)]
pub struct MailFrom(pub String);

/// The notifications module.
pub struct Notify {
    mailer: Mailer,
    from: String,
}

impl Notify {
    /// Sends through `mailer` from `from` (an address the platform may send
    /// as, e.g. `shops@soat.example`), with each shop's name shown.
    pub fn new(mailer: Mailer, from: impl Into<String>) -> Self {
        Self {
            mailer,
            from: from.into(),
        }
    }
}

impl Module for Notify {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "customers",
            "payments",
            "orders",
            "giftcards",
        ]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[NOTIFICATIONS_READ, NOTIFICATIONS_MANAGE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(Mailings)]
    }

    fn personal_data(&self) -> &'static [soat_module::PersonalData] {
        &[soat_module::PersonalData {
            table: "email_messages",
            matches: "lower(recipient) = $3",
            export: &["template", "subject", "status", "created_at", "sent_at"],
            erase: soat_module::Erase::Delete,
        }]
    }

    fn retention(&self) -> &'static [soat_module::Retention] {
        &[soat_module::Retention {
            key: "sent_emails",
            label: "Emails sent to customers (their copies)",
            table: "email_messages",
            expired: "status <> 'pending' AND created_at < now() - make_interval(days => $2)",
            default_days: Some(730),
            min_days: 30,
        }]
    }

    fn mount(&self, app: App) -> App {
        app.mailer(self.mailer.clone())
            .state(MailFrom(self.from.clone()))
            .mount((
                get_settings,
                put_settings,
                list_templates,
                get_template,
                put_template,
                reset_template,
                preview_template,
                list_messages,
                get_message,
                resend_message,
            ))
    }
}

/// How a shop's email goes out.
#[model]
#[derive(sqlx::FromRow)]
pub struct NotificationSettings {
    /// Shown as the sender; empty for the shop's name.
    pub sender_name: String,
    /// Where replies go; null for nowhere.
    pub reply_to: Option<String>,
    /// Where new-order notices for staff go; null for none.
    pub staff_email: Option<String>,
    /// Templates not sent, e.g. `["payment_received"]`.
    pub disabled: Vec<String>,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            sender_name: String::new(),
            reply_to: None,
            staff_email: None,
            disabled: vec!["payment_received".to_owned()],
        }
    }
}

/// Body of changing the settings.
#[model]
pub struct SettingsInput {
    #[validate(length(max = 100))]
    #[serde(default)]
    pub sender_name: String,
    #[validate(email, length(max = 254))]
    pub reply_to: Option<String>,
    #[validate(email, length(max = 254))]
    pub staff_email: Option<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}

/// A template as the shop sends it.
#[model]
pub struct EmailTemplate {
    /// e.g. `order_confirmation`.
    pub key: String,
    pub description: String,
    /// Whether the shop reworded it.
    pub customized: bool,
    pub enabled: bool,
    pub subject: String,
    pub text: String,
    /// Empty sends text only.
    pub html: String,
}

/// Body of rewording a template.
#[model]
pub struct TemplateInput {
    #[validate(length(min = 1, max = 300))]
    pub subject: String,
    #[validate(length(min = 1, max = 65536))]
    pub text: String,
    #[validate(length(max = 65536))]
    #[serde(default)]
    pub html: String,
}

/// Body of previewing a template: unsaved wording, or the current one.
#[model]
pub struct PreviewInput {
    pub subject: Option<String>,
    pub text: Option<String>,
    pub html: Option<String>,
}

/// A rendered email.
#[model]
pub struct Rendered {
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// An email sent, or tried.
#[model]
#[derive(sqlx::FromRow)]
pub struct SentEmail {
    /// e.g. `eml_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub template: String,
    pub order_id: Option<String>,
    pub recipient: String,
    pub subject: String,
    pub text_body: String,
    pub html_body: String,
    /// `pending`, `sent` or `failed`.
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub sent_at: Option<String>,
}

const MESSAGE_COLUMNS: &str = "id, template, order_id, recipient, subject, text_body, html_body, \
     status, attempts, last_error, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(sent_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS sent_at";

async fn settings(conn: &mut PgConnection, scope: &impl Scope) -> Result<NotificationSettings> {
    let s = sqlx::query_as::<_, NotificationSettings>(
        "SELECT sender_name, reply_to, staff_email, disabled FROM notification_settings \
         WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(s.unwrap_or_default())
}

async fn shop_name(conn: &mut PgConnection, scope: &impl Scope) -> Result<String> {
    let (name,): (String,) = sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
        .bind(scope.tenant_id())
        .fetch_one(&mut *conn)
        .await?;
    Ok(name)
}

/// The shop's wording of `key`, or the built-in one.
async fn wording(
    conn: &mut PgConnection,
    scope: &impl Scope,
    b: &Builtin,
) -> Result<(bool, String, String, String)> {
    let own: Option<(String, String, String)> = sqlx::query_as(
        "SELECT subject, text_body, html_body FROM email_templates \
         WHERE tenant_id = $1 AND key = $2",
    )
    .bind(scope.tenant_id())
    .bind(b.key)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match own {
        Some((s, t, h)) => (true, s, t, h),
        None => (false, b.subject.into(), b.text.into(), b.html.into()),
    })
}

fn parse(source: &str, what: &str) -> Result<Template> {
    let t = Template::parse(source).map_err(|e| {
        Error::unprocessable(format!("the {what}: {e}")).with_code("invalid_template")
    })?;
    if let Some(p) = t.partials().first() {
        return Err(Error::unprocessable(format!(
            "the {what}: emails have no partials, so {{{{> {p} }}}} cannot be used"
        ))
        .with_code("invalid_template"));
    }
    Ok(t)
}

fn render(subject: &str, text: &str, html: &str, data: &Value) -> Result<Rendered> {
    let subject = parse(subject, "subject")?.render(data, false);
    Ok(Rendered {
        // One line, whatever the data holds.
        subject: subject.replace(['\r', '\n'], " ").trim().to_owned(),
        text: parse(text, "text")?.render(data, false),
        html: parse(html, "html")?.render(data, true),
    })
}

fn known(key: &str) -> Result<&'static Builtin> {
    defaults::builtin(key).ok_or_else(Error::not_found)
}

/// The shop's email settings (`notifications:read`).
#[get("/api/notifications/settings", tag = "notifications")]
async fn get_settings(db: Db, tenant: Tenant) -> Result<Json<NotificationSettings>> {
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Set the sender name, reply-to and staff addresses, and which templates
/// are off (`notifications:manage`). Publishes
/// `notification_settings.updated`.
#[put("/api/notifications/settings", tag = "notifications")]
async fn put_settings(
    db: Db,
    tenant: Tenant,
    Valid(Json(s)): Valid<Json<SettingsInput>>,
) -> Result<Json<NotificationSettings>> {
    if let Some(k) = s.disabled.iter().find(|k| defaults::builtin(k).is_none()) {
        return Err(
            Error::unprocessable(format!("{k} is not a template")).with_code("unknown_template")
        );
    }
    let name = s.sender_name.replace(['"', '<', '>', '\r', '\n', '\\'], "");
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_MANAGE).await?;
    let saved = sqlx::query_as::<_, NotificationSettings>(
        "INSERT INTO notification_settings (tenant_id, sender_name, reply_to, staff_email, \
         disabled) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id) DO UPDATE SET \
         sender_name = EXCLUDED.sender_name, reply_to = EXCLUDED.reply_to, \
         staff_email = EXCLUDED.staff_email, disabled = EXCLUDED.disabled, updated_at = now() \
         RETURNING sender_name, reply_to, staff_email, disabled",
    )
    .bind(tenant.id())
    .bind(name.trim())
    .bind(s.reply_to.as_deref().map(str::trim))
    .bind(s.staff_email.as_deref().map(str::trim))
    .bind(&s.disabled)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "notification_settings.updated", &saved)
        .await?;
    tx.commit().await?;
    Ok(Json(saved))
}

async fn template_view(
    conn: &mut PgConnection,
    scope: &impl Scope,
    b: &Builtin,
    s: &NotificationSettings,
) -> Result<EmailTemplate> {
    let (customized, subject, text, html) = wording(conn, scope, b).await?;
    Ok(EmailTemplate {
        key: b.key.into(),
        description: b.description.into(),
        customized,
        enabled: !s.disabled.iter().any(|k| k == b.key),
        subject,
        text,
        html,
    })
}

/// Every template the shop can send, with its current wording
/// (`notifications:read`).
#[get("/api/notifications/templates", tag = "notifications")]
async fn list_templates(db: Db, tenant: Tenant) -> Result<Json<Vec<EmailTemplate>>> {
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    let mut all = Vec::with_capacity(BUILTINS.len());
    for b in BUILTINS {
        all.push(template_view(&mut tx, &tenant, b, &s).await?);
    }
    tx.commit().await?;
    Ok(Json(all))
}

/// One template (`notifications:read`).
#[get("/api/notifications/templates/{key}", tag = "notifications")]
async fn get_template(
    db: Db,
    tenant: Tenant,
    Path(key): Path<String>,
) -> Result<Json<EmailTemplate>> {
    let b = known(&key)?;
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    let t = template_view(&mut tx, &tenant, b, &s).await?;
    tx.commit().await?;
    Ok(Json(t))
}

/// Reword a template (`notifications:manage`). A template that does not
/// parse is a 422 `invalid_template`. Publishes `email_template.updated`.
#[put("/api/notifications/templates/{key}", tag = "notifications")]
async fn put_template(
    db: Db,
    tenant: Tenant,
    Path(key): Path<String>,
    Valid(Json(t)): Valid<Json<TemplateInput>>,
) -> Result<Json<EmailTemplate>> {
    let b = known(&key)?;
    render(&t.subject, &t.text, &t.html, &Value::Null)?;
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_MANAGE).await?;
    sqlx::query(
        "INSERT INTO email_templates (tenant_id, key, subject, text_body, html_body) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, key) DO UPDATE SET \
         subject = EXCLUDED.subject, text_body = EXCLUDED.text_body, \
         html_body = EXCLUDED.html_body, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(b.key)
    .bind(&t.subject)
    .bind(&t.text)
    .bind(&t.html)
    .execute(&mut *tx)
    .await?;
    let s = settings(&mut tx, &tenant).await?;
    let view = template_view(&mut tx, &tenant, b, &s).await?;
    tenant
        .publish(&mut tx, "email_template.updated", &view)
        .await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Go back to the built-in wording (`notifications:manage`). Publishes
/// `email_template.reset`.
#[delete("/api/notifications/templates/{key}", tag = "notifications")]
async fn reset_template(
    db: Db,
    tenant: Tenant,
    Path(key): Path<String>,
) -> Result<Json<EmailTemplate>> {
    let b = known(&key)?;
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_MANAGE).await?;
    sqlx::query("DELETE FROM email_templates WHERE tenant_id = $1 AND key = $2")
        .bind(tenant.id())
        .bind(b.key)
        .execute(&mut *tx)
        .await?;
    let s = settings(&mut tx, &tenant).await?;
    let view = template_view(&mut tx, &tenant, b, &s).await?;
    tenant
        .publish(&mut tx, "email_template.reset", &json!({ "key": b.key }))
        .await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// A template rendered with made-up order data: the wording sent (`{}`
/// for the current one) (`notifications:read`).
#[post("/api/notifications/templates/{key}/preview", tag = "notifications")]
async fn preview_template(
    db: Db,
    tenant: Tenant,
    Path(key): Path<String>,
    Json(p): Json<PreviewInput>,
) -> Result<Json<Rendered>> {
    let b = known(&key)?;
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_READ).await?;
    let (_, subject, text, html) = wording(&mut tx, &tenant, b).await?;
    let data = context::sample(&shop_name(&mut tx, &tenant).await?);
    tx.commit().await?;
    let r = render(
        p.subject.as_deref().unwrap_or(&subject),
        p.text.as_deref().unwrap_or(&text),
        p.html.as_deref().unwrap_or(&html),
        &data,
    )?;
    Ok(Json(r))
}

/// Emails sent or tried, newest first (`notifications:read`). Filter with
/// `?template=`, `?status=`, `?recipient=` or `?order_id=`.
#[get("/api/notifications/messages", tag = "notifications")]
async fn list_messages(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<SentEmail>>> {
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_READ).await?;
    let page = TenantListing::new("email_messages", MESSAGE_COLUMNS)
        .sortable(["id"])
        .default_sort("-id")
        .filter_text("template")
        .filter_text("status")
        .filter_text("recipient")
        .filter_text("order_id")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn message(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<SentEmail> {
    let id = EmailMessageId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, SentEmail>(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM email_messages WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// One email (`notifications:read`).
#[get("/api/notifications/messages/{id}", tag = "notifications")]
async fn get_message(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<SentEmail>> {
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_READ).await?;
    let m = message(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Send an email again, as it was (`notifications:manage`). A mail server
/// that refuses it is a 503, and the log says why. Publishes `email.resent`.
#[post("/api/notifications/messages/{id}/resend", tag = "notifications")]
async fn resend_message(
    db: Db,
    tenant: Tenant,
    mailer: State<Mailer>,
    from: State<MailFrom>,
    Path(id): Path<String>,
) -> Result<Json<SentEmail>> {
    let mut tx = tenant.begin_for(&db, &NOTIFICATIONS_MANAGE).await?;
    let m = message(&mut tx, &tenant, &id).await?;
    if SECRET_TEMPLATES.contains(&m.template.as_str()) {
        return Err(Error::unprocessable(
            "an email with an access link or a gift card code cannot be resent",
        )
        .with_code("not_resendable"));
    }
    let s = settings(&mut tx, &tenant).await?;
    let sender = sender(&s, &shop_name(&mut tx, &tenant).await?, &from.0 .0);
    let outcome = deliver(&mailer.0, &sender, s.reply_to.as_deref(), &m).await;
    record(&mut tx, &tenant, &m.id, &outcome).await?;
    let m = message(&mut tx, &tenant, &id).await?;
    tenant
        .publish(
            &mut tx,
            "email.resent",
            &json!({ "id": m.id, "template": m.template, "recipient": m.recipient, "status": m.status }),
        )
        .await?;
    tx.commit().await?;
    match outcome {
        Ok(()) => Ok(Json(m)),
        Err(e) => Err(
            Error::service_unavailable(format!("the mail server refused it: {e}"))
                .with_code("mail_failed"),
        ),
    }
}

/// Who shop `scope`'s email comes from (`"Shop name" <address>`, with the
/// platform's sending `address`) and where replies go, for modules that
/// send email of their own.
///
/// # Errors
///
/// Database errors.
pub async fn shop_sender(
    conn: &mut PgConnection,
    scope: &impl Scope,
    address: &str,
) -> Result<(String, Option<String>)> {
    let s = settings(conn, scope).await?;
    let shop = shop_name(conn, scope).await?;
    Ok((sender(&s, &shop, address), s.reply_to))
}

/// `"Shop name" <platform address>`.
fn sender(s: &NotificationSettings, shop: &str, address: &str) -> String {
    let name = if s.sender_name.is_empty() {
        shop
    } else {
        &s.sender_name
    };
    let name = name.replace(['"', '<', '>', '\r', '\n', '\\'], "");
    if name.trim().is_empty() {
        address.to_owned()
    } else {
        format!("\"{}\" <{address}>", name.trim())
    }
}

async fn deliver(
    mailer: &Mailer,
    from: &str,
    reply_to: Option<&str>,
    m: &SentEmail,
) -> std::result::Result<(), String> {
    let mut email = Email::to(&m.recipient)
        .from(from)
        .subject(&m.subject)
        .text(&m.text_body);
    if !m.html_body.is_empty() {
        email = email.html(&m.html_body);
    }
    if let Some(r) = reply_to {
        email = email.reply_to(r);
    }
    mailer.send(email).await.map_err(|e| e.to_string())
}

/// Notes how sending went.
async fn record(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
    outcome: &std::result::Result<(), String>,
) -> Result<()> {
    let (status, error) = match outcome {
        Ok(()) => ("sent", None),
        Err(e) => ("failed", Some(e.as_str())),
    };
    sqlx::query(
        "UPDATE email_messages SET status = $3, attempts = attempts + 1, last_error = $4, \
         sent_at = CASE WHEN $3 = 'sent' THEN now() ELSE sent_at END \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .bind(status)
    .bind(error)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The shop an event belongs to; the system acts.
struct EventScope(String);

impl Scope for EventScope {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// A customer asked for a link to set a password.
const ACCESS_REQUESTED: &str = "customer.access_requested";
/// The emails that carry such a link.
const ACCESS_TEMPLATES: &[&str] = &["password_reset", "account_activation"];
/// A gift card bought with an order was issued.
const GIFT_CARD_ISSUED: &str = "gift_card.issued";
/// The emails that carry a secret (an access link, a gift card code):
/// their bodies are not kept once sent, and they cannot be resent.
const SECRET_TEMPLATES: &[&str] = &[
    "password_reset",
    "account_activation",
    "gift_card_issued",
    "cart_recovery",
];
/// A buyer left a cart.
const CART_ABANDONED: &str = "cart.abandoned";

/// Which emails an event sends: `(template, what it is about)`.
fn templates_for(kind: &str) -> &'static [&'static str] {
    match kind {
        "order.placed" => &["order_confirmation", "staff_new_order"],
        "order.paid" => &["payment_received"],
        "order.canceled" => &["order_canceled"],
        "fulfillment.created" => &["shipping_confirmation"],
        "payment.refunded" => &["refund_issued"],
        ACCESS_REQUESTED => ACCESS_TEMPLATES,
        GIFT_CARD_ISSUED => &["gift_card_issued"],
        CART_ABANDONED => &["cart_recovery"],
        _ => &[],
    }
}

/// Sends the emails order events call for.
struct Mailings;

impl Mailings {
    /// The order an event is about.
    async fn order(
        conn: &mut PgConnection,
        scope: &EventScope,
        event: &Event,
    ) -> Result<Option<Order>> {
        let id = match event.kind.as_str() {
            "fulfillment.created" => event.payload["order_id"].as_str().map(str::to_owned),
            "payment.refunded" => match event.payload["payment_id"].as_str() {
                Some(p) => soat_payments::get(conn, scope, p)
                    .await?
                    .map(|p| p.reference),
                None => None,
            },
            _ => event.payload["id"].as_str().map(str::to_owned),
        };
        match id {
            Some(id) => soat_orders::load(conn, scope, &id).await,
            None => Ok(None),
        }
    }
}

impl Subscriber for Mailings {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn wants(&self, kind: &str) -> bool {
        !templates_for(kind).is_empty()
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let pool: PgPool = res.require()?;
            let mailer: Mailer = res.require()?;
            let MailFrom(address) = res.require()?;
            let scope = EventScope(event.tenant_id.clone());
            let begin = || async {
                let mut tx = pool.begin().await?;
                sqlx::query("SELECT set_config('soat.tenant', $1, true)")
                    .bind(&event.tenant_id)
                    .execute(&mut *tx)
                    .await?;
                Ok::<_, Error>(tx)
            };

            // Write the emails down, once per event.
            let mut tx = begin().await?;
            let s = settings(&mut tx, &scope).await?;
            let shop = shop_name(&mut tx, &scope).await?;
            let ids = if event.kind == ACCESS_REQUESTED {
                access_emails(&mut tx, &scope, event, &shop).await?
            } else if event.kind == GIFT_CARD_ISSUED {
                // The buyer's only copy of what they paid for: always sent.
                gift_card_emails(&mut tx, &scope, event, &shop).await?
            } else if event.kind == CART_ABANDONED {
                if s.disabled.iter().any(|k| k == "cart_recovery") {
                    soat_cart::recovery::recovery_mail(
                        &mut tx,
                        &scope,
                        event.payload["id"].as_str().unwrap_or_default(),
                    )
                    .await?;
                    tx.commit().await?;
                    return Ok(());
                }
                cart_emails(&mut tx, &scope, event, &shop).await?
            } else {
                let Some(order) = Self::order(&mut tx, &scope, event).await? else {
                    return Ok(());
                };
                order_emails(&mut tx, &scope, event, &s, &shop, &order).await?
            };
            tx.commit().await?;

            // Send them, noting each outcome; a failure retries the event.
            let sender = sender(&s, &shop, &address);
            let mut failed = None;
            for id in ids {
                let mut tx = begin().await?;
                let m = message(&mut tx, &scope, &id).await?;
                let outcome = deliver(&mailer, &sender, s.reply_to.as_deref(), &m).await;
                record(&mut tx, &scope, &id, &outcome).await?;
                if outcome.is_ok() && SECRET_TEMPLATES.contains(&m.template.as_str()) {
                    redact(&mut tx, &scope, &id, &m.template).await?;
                }
                tx.commit().await?;
                if let Err(e) = outcome {
                    failed = Some(e);
                }
            }
            match failed {
                Some(e) => Err(Error::service_unavailable(format!(
                    "cannot send an email: {e}"
                ))),
                None => Ok(()),
            }
        })
    }
}

/// Writes down the emails an order event calls for; the ids still to send.
async fn order_emails(
    tx: &mut PgConnection,
    scope: &EventScope,
    event: &Event,
    s: &NotificationSettings,
    shop: &str,
    order: &Order,
) -> Result<Vec<String>> {
    let mut data = json!({
        "shop": { "name": shop },
        "order": context::order(order),
    });
    match event.kind.as_str() {
        "fulfillment.created" => {
            data["fulfillment"] = context::fulfillment(order, &event.payload["fulfillment"]);
        }
        "payment.refunded" => data["refund"] = context::refund(order, &event.payload),
        _ => {}
    }
    let mut ids = Vec::new();
    for key in templates_for(&event.kind) {
        if s.disabled.iter().any(|k| k == key) {
            continue;
        }
        let recipient = if *key == "staff_new_order" {
            match &s.staff_email {
                Some(e) => e.clone(),
                None => continue,
            }
        } else {
            order.email.clone()
        };
        let r = rendered(tx, scope, key, &data).await?;
        ids.extend(write_message(tx, event, key, Some(&order.id), &recipient, &r).await?);
    }
    Ok(ids)
}

/// Writes down the email with a customer's access link. The token is read
/// once; a retried event finds the email already written.
async fn access_emails(
    tx: &mut PgConnection,
    scope: &EventScope,
    event: &Event,
    shop: &str,
) -> Result<Vec<String>> {
    let Some(link) = event.payload["id"].as_str() else {
        return Ok(Vec::new());
    };
    let Some(mail) = soat_customers::access::access_mail(tx, scope, link).await? else {
        // Written before (a retry) or no longer usable.
        let unsent: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM email_messages WHERE tenant_id = $1 AND event_id = $2 \
             AND status <> 'sent'",
        )
        .bind(&event.tenant_id)
        .bind(&event.id)
        .fetch_all(&mut *tx)
        .await?;
        return Ok(unsent.into_iter().map(|(id,)| id).collect());
    };
    let key = if mail.purpose == "reset" {
        "password_reset"
    } else {
        "account_activation"
    };
    let data = json!({
        "shop": { "name": shop },
        "customer": { "first_name": mail.first_name, "email": mail.email },
        "access": {
            "link": mail.link,
            "code": mail.token,
            "minutes": soat_customers::access::LINK_MINUTES,
        },
    });
    let r = rendered(tx, scope, key, &data).await?;
    Ok(write_message(tx, event, key, None, &mail.email, &r)
        .await?
        .into_iter()
        .collect())
}

/// Writes down the email with a gift card's code for its buyer. The code
/// is read once; a retried event finds the email already written.
async fn gift_card_emails(
    tx: &mut PgConnection,
    scope: &EventScope,
    event: &Event,
    shop: &str,
) -> Result<Vec<String>> {
    let Some(card) = event.payload["id"].as_str() else {
        return Ok(Vec::new());
    };
    let Some(mail) = soat_giftcards::issued_mail(tx, scope, card).await? else {
        let unsent: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM email_messages WHERE tenant_id = $1 AND event_id = $2 \
             AND status <> 'sent'",
        )
        .bind(&event.tenant_id)
        .bind(&event.id)
        .fetch_all(&mut *tx)
        .await?;
        return Ok(unsent.into_iter().map(|(id,)| id).collect());
    };
    if mail.email.is_empty() {
        return Ok(Vec::new());
    }
    let order = match &mail.card.order_id {
        Some(o) => soat_orders::load(tx, scope, o).await?,
        None => None,
    };
    let amount = match soat_core::Currency::parse(&mail.card.currency) {
        Ok(c) => c.format(mail.card.initial_minor),
        Err(_) => format!("{} {}", mail.card.initial_minor, mail.card.currency),
    };
    let data = json!({
        "shop": { "name": shop },
        "order": order.as_ref().map(context::order),
        "gift_card": {
            "code": mail.code,
            "amount": amount,
            "expires_at": mail.card.expires_at,
        },
    });
    let key = "gift_card_issued";
    let r = rendered(tx, scope, key, &data).await?;
    Ok(write_message(
        tx,
        event,
        key,
        mail.card.order_id.as_deref(),
        &mail.email,
        &r,
    )
    .await?
    .into_iter()
    .collect())
}

/// Writes down the email about a cart its buyer left. The link's token is
/// read once; a retried event finds the email already written.
async fn cart_emails(
    tx: &mut PgConnection,
    scope: &EventScope,
    event: &Event,
    shop: &str,
) -> Result<Vec<String>> {
    let Some(cart) = event.payload["id"].as_str() else {
        return Ok(Vec::new());
    };
    let Some(mail) = soat_cart::recovery::recovery_mail(tx, scope, cart).await? else {
        let unsent: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM email_messages WHERE tenant_id = $1 AND event_id = $2 \
             AND status <> 'sent'",
        )
        .bind(&event.tenant_id)
        .bind(&event.id)
        .fetch_all(&mut *tx)
        .await?;
        return Ok(unsent.into_iter().map(|(id,)| id).collect());
    };
    let domain: Option<(String,)> = sqlx::query_as(
        "SELECT domain FROM tenant_domains WHERE tenant_id = $1 ORDER BY domain LIMIT 1",
    )
    .bind(&event.tenant_id)
    .fetch_optional(&mut *tx)
    .await?;
    let link = domain.map(|(d,)| format!("https://{d}/cart/recover?token={}", mail.token));
    let lines: Vec<Value> = mail
        .cart
        .lines
        .iter()
        .map(|l| json!({ "title": l.title.as_deref().unwrap_or(&l.item_id), "quantity": l.quantity }))
        .collect();
    let data = json!({
        "shop": { "name": shop },
        "cart": { "link": link, "token": mail.token, "lines": lines },
    });
    let key = "cart_recovery";
    let r = rendered(tx, scope, key, &data).await?;
    Ok(write_message(tx, event, key, None, &mail.email, &r)
        .await?
        .into_iter()
        .collect())
}

/// Template `key` in the shop's wording (the built-in one if the shop's
/// does not render).
async fn rendered(
    tx: &mut PgConnection,
    scope: &EventScope,
    key: &str,
    data: &Value,
) -> Result<Rendered> {
    let b = known(key)?;
    let (_, subject, text, html) = wording(tx, scope, b).await?;
    render(&subject, &text, &html, data).or_else(|_| render(b.subject, b.text, b.html, data))
}

/// Logs an email for `event`, once per template; its id while unsent.
async fn write_message(
    tx: &mut PgConnection,
    event: &Event,
    key: &str,
    order_id: Option<&str>,
    recipient: &str,
    r: &Rendered,
) -> Result<Option<String>> {
    sqlx::query(
        "INSERT INTO email_messages (id, tenant_id, template, event_id, order_id, \
         recipient, subject, text_body, html_body) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (tenant_id, event_id, template) DO NOTHING",
    )
    .bind(EmailMessageId::new().as_str())
    .bind(&event.tenant_id)
    .bind(key)
    .bind(&event.id)
    .bind(order_id)
    .bind(recipient)
    .bind(&r.subject)
    .bind(&r.text)
    .bind(&r.html)
    .execute(&mut *tx)
    .await?;
    let id: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM email_messages WHERE tenant_id = $1 AND event_id = $2 \
         AND template = $3 AND status <> 'sent'",
    )
    .bind(&event.tenant_id)
    .bind(&event.id)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(id.map(|(id,)| id))
}

/// Drops the body of a sent email that carried a secret.
async fn redact(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
    template: &str,
) -> Result<()> {
    let note = if template == "gift_card_issued" {
        "This email held a gift card code; it is not kept."
    } else if template == "cart_recovery" {
        "This email held a link to the buyer's cart; it is not kept."
    } else {
        "This email held a link to set a password; it is not kept."
    };
    sqlx::query(
        "UPDATE email_messages SET text_body = $3, html_body = '' \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .bind(note)
    .execute(conn)
    .await?;
    Ok(())
}
