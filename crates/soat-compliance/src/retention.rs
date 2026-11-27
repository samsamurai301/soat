//! Data retention: personal data is kept no longer than needed (GDPR art.
//! 5(1)(e)).
//!
//! Modules declare how long their tables' old rows are kept
//! ([`soat_module::Retention`]): carts never checked out, expired sign-ins,
//! copies of sent emails, the event log, cookie choices. Each shop can
//! shorten or lengthen each time (never below the rule's minimum) or keep
//! the rows, and can erase customers who have neither ordered nor signed in
//! for a time, the same way staff erase one by hand. An hourly task sweeps
//! each shop once a day; staff can see what is due and sweep now.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Map, Value};
use soat_module::{PersonalDataMap, RetentionMap};
use soat_tenancy::{EventShop, Scope, Tenant, TenantTx};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::Resources;

use crate::consent::PRIVACY_SETTINGS;
use crate::{erase, PRIVACY_ERASE, PRIVACY_READ};

/// The rule that erases customers who neither ordered nor signed in.
pub const INACTIVE_CUSTOMERS: &str = "inactive_customers";
const INACTIVE_LABEL: &str = "Customers who neither ordered nor signed in (erased)";
const INACTIVE_MIN_DAYS: i32 = 365;
/// Most customers erased by one sweep; the rest wait for the next.
const ERASE_PER_SWEEP: i64 = 200;
/// Most days a rule can be set to (a hundred years).
const MAX_DAYS: i32 = 36_500;
/// How often the task looks for shops due a sweep.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

/// One retention rule of the shop.
#[model]
pub struct RetentionRule {
    /// e.g. `abandoned_carts`.
    pub key: String,
    /// The module that declares it.
    pub module: String,
    pub label: String,
    /// What the shop keeps the rows for, in days; `null` keeps them.
    pub days: Option<i32>,
    pub default_days: Option<i32>,
    pub min_days: i32,
    /// Rows (or customers) a sweep would remove now.
    pub due: i64,
}

/// The shop's retention rules and its last sweep.
#[model]
pub struct RetentionView {
    pub rules: Vec<RetentionRule>,
    /// RFC 3339, UTC; `null` before the first sweep.
    pub last_run_at: Option<String>,
    /// What the last sweep removed, by rule.
    pub last_report: BTreeMap<String, i64>,
}

/// Body of setting how long data is kept.
#[model]
pub struct RetentionInput {
    /// Days by rule key; `null` keeps the rows. Rules left out keep their
    /// current setting.
    pub days: BTreeMap<String, Option<i32>>,
    /// Rule keys to set back to their default.
    #[serde(default)]
    pub reset: Vec<String>,
}

/// What a sweep removed.
#[model]
pub struct SweepReport {
    /// Rows deleted (customers erased, for `inactive_customers`) by rule.
    pub removed: BTreeMap<String, i64>,
    pub total: i64,
}

struct Rule {
    key: &'static str,
    module: &'static str,
    label: &'static str,
    default_days: Option<i32>,
    min_days: i32,
    /// `None` for inactive customers, which are erased, not deleted.
    sql: Option<(&'static str, &'static str)>,
}

fn rules(map: &RetentionMap) -> Vec<Rule> {
    let mut all: Vec<Rule> = map
        .all()
        .iter()
        .map(|(module, r)| Rule {
            key: r.key,
            module,
            label: r.label,
            default_days: r.default_days,
            min_days: r.min_days,
            sql: Some((r.table, r.expired)),
        })
        .collect();
    all.push(Rule {
        key: INACTIVE_CUSTOMERS,
        module: "compliance",
        label: INACTIVE_LABEL,
        default_days: None,
        min_days: INACTIVE_MIN_DAYS,
        sql: None,
    });
    all
}

/// The days a rule is set to: the shop's choice (`null` keeps), else the
/// rule's default.
fn days_of(stored: &Map<String, Value>, key: &str, default_days: Option<i32>) -> Option<i32> {
    match stored.get(key) {
        Some(v) => v.as_i64().and_then(|d| i32::try_from(d).ok()),
        None => default_days,
    }
}

struct Stored {
    days: Map<String, Value>,
    last_run_at: Option<String>,
    last_report: BTreeMap<String, i64>,
}

async fn stored(conn: &mut PgConnection, scope: &impl Scope) -> Result<Stored> {
    let row: Option<(Value, Option<String>, Value)> = sqlx::query_as(
        "SELECT days, to_char(last_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
         last_report FROM retention_settings WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(conn)
    .await?;
    let (days, last_run_at, report) = row.unwrap_or((json!({}), None, json!({})));
    Ok(Stored {
        days: days.as_object().cloned().unwrap_or_default(),
        last_run_at,
        last_report: serde_json::from_value(report).unwrap_or_default(),
    })
}

/// Customers of the shop with no order and no sign-in for `days`, who
/// joined before that and hold no running subscription.
fn inactive_sql(select: &str, subscriptions: bool) -> String {
    let subs = if subscriptions {
        " AND NOT EXISTS (SELECT 1 FROM subscriptions u WHERE u.tenant_id = $1 \
         AND u.customer_id = c.id AND u.status <> 'canceled')"
    } else {
        ""
    };
    format!(
        "SELECT {select} FROM customers c WHERE c.tenant_id = $1 \
         AND c.created_at < now() - make_interval(days => $2) \
         AND NOT EXISTS (SELECT 1 FROM orders o WHERE o.tenant_id = $1 \
           AND (o.customer_id = c.id OR lower(o.email) = lower(c.email)) \
           AND o.placed_at >= now() - make_interval(days => $2)) \
         AND NOT EXISTS (SELECT 1 FROM customer_sessions s WHERE s.tenant_id = $1 \
           AND s.customer_id = c.id AND s.created_at >= now() - make_interval(days => $2)){subs}"
    )
}

async fn has_subscriptions(conn: &mut PgConnection) -> Result<bool> {
    let (found,): (bool,) = sqlx::query_as("SELECT to_regclass('subscriptions') IS NOT NULL")
        .fetch_one(conn)
        .await?;
    Ok(found)
}

/// How many rows (or customers) `rule` would remove at `days`.
async fn due(conn: &mut PgConnection, scope: &impl Scope, rule: &Rule, days: i32) -> Result<i64> {
    let sql = match rule.sql {
        Some((table, expired)) => {
            format!("SELECT count(*) FROM {table} WHERE tenant_id = $1 AND ({expired})")
        }
        None => {
            let subs = has_subscriptions(conn).await?;
            inactive_sql("count(*)", subs)
        }
    };
    let (n,): (i64,) = sqlx::query_as(&sql)
        .bind(scope.tenant_id())
        .bind(days)
        .fetch_one(conn)
        .await?;
    Ok(n)
}

/// The shop's rules, with what a sweep would remove now.
///
/// # Errors
///
/// Database errors.
pub async fn view(
    conn: &mut PgConnection,
    scope: &impl Scope,
    map: &RetentionMap,
) -> Result<RetentionView> {
    let s = stored(conn, scope).await?;
    let mut out = Vec::new();
    for rule in rules(map) {
        let days = days_of(&s.days, rule.key, rule.default_days);
        let n = match days {
            Some(d) => due(conn, scope, &rule, d).await?,
            None => 0,
        };
        out.push(RetentionRule {
            key: rule.key.to_owned(),
            module: rule.module.to_owned(),
            label: rule.label.to_owned(),
            days,
            default_days: rule.default_days,
            min_days: rule.min_days,
            due: n,
        });
    }
    Ok(RetentionView {
        rules: out,
        last_run_at: s.last_run_at,
        last_report: s.last_report,
    })
}

/// Removes what the shop's rules say is past its time, records the sweep
/// and publishes `retention.swept` when something went (or `always`).
/// Inactive customers are erased like by hand, each publishing
/// `customer.erased`.
///
/// # Errors
///
/// Database errors.
pub async fn sweep(
    tx: &mut TenantTx,
    scope: &impl Scope,
    map: &RetentionMap,
    personal: &PersonalDataMap,
    always: bool,
) -> Result<SweepReport> {
    let s = stored(tx, scope).await?;
    let mut removed = BTreeMap::new();
    for rule in rules(map) {
        let Some(days) = days_of(&s.days, rule.key, rule.default_days) else {
            continue;
        };
        // A setting saved below a minimum that later rose still waits.
        let days = days.max(rule.min_days);
        let n = match rule.sql {
            Some((table, expired)) => {
                let done = sqlx::query(&format!(
                    "DELETE FROM {table} WHERE tenant_id = $1 AND ({expired})"
                ))
                .bind(scope.tenant_id())
                .bind(days)
                .execute(&mut **tx)
                .await?;
                i64::try_from(done.rows_affected()).unwrap_or(i64::MAX)
            }
            None => {
                let subs = has_subscriptions(tx).await?;
                let who: Vec<(String, String)> = sqlx::query_as(&format!(
                    "{} ORDER BY c.created_at LIMIT {ERASE_PER_SWEEP}",
                    inactive_sql("c.id, c.email", subs)
                ))
                .bind(scope.tenant_id())
                .bind(days)
                .fetch_all(&mut **tx)
                .await?;
                for (id, email) in &who {
                    let report = erase(tx, scope, personal, id, email).await?;
                    let rows: i64 = report
                        .tables
                        .iter()
                        .filter(|t| t.action != "kept")
                        .map(|t| t.rows)
                        .sum();
                    scope
                        .publish(
                            tx,
                            "customer.erased",
                            &json!({
                                "id": id, "rows": rows,
                                "events_redacted": report.events_redacted,
                                "reason": INACTIVE_CUSTOMERS,
                            }),
                        )
                        .await?;
                }
                i64::try_from(who.len()).unwrap_or(i64::MAX)
            }
        };
        if n > 0 {
            removed.insert(rule.key.to_owned(), n);
        }
    }
    let total = removed.values().sum();
    let report = SweepReport { removed, total };
    sqlx::query(
        "INSERT INTO retention_settings (tenant_id, last_run_at, last_report) \
         VALUES ($1, now(), $2) ON CONFLICT (tenant_id) DO UPDATE SET \
         last_run_at = now(), last_report = EXCLUDED.last_report",
    )
    .bind(scope.tenant_id())
    .bind(json!(report.removed))
    .execute(&mut **tx)
    .await?;
    if always || total > 0 {
        scope.publish(tx, "retention.swept", &report).await?;
    }
    Ok(report)
}

/// Sweeps every shop not swept in the last day.
///
/// # Errors
///
/// Database errors.
pub async fn sweep_due(res: &Resources) -> Result<usize> {
    let pool: PgPool = res.require()?;
    let map: RetentionMap = res.require()?;
    let personal: PersonalDataMap = res.require()?;
    // `retention_settings` is per shop; the tenants list is not.
    let shops: Vec<(String,)> = sqlx::query_as("SELECT id FROM tenants ORDER BY id")
        .fetch_all(&pool)
        .await?;
    let mut swept = 0;
    for (shop,) in shops {
        let scope = EventShop(shop);
        let mut tx = scope.begin_on(&pool).await?;
        let (recent,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM retention_settings WHERE tenant_id = $1 \
             AND last_run_at > now() - interval '23 hours')",
        )
        .bind(scope.tenant_id())
        .fetch_one(&mut *tx)
        .await?;
        if recent {
            continue;
        }
        sweep(&mut tx, &scope, &map, &personal, false).await?;
        tx.commit().await?;
        swept += 1;
    }
    Ok(swept)
}

pub(crate) fn schedule(app: App) -> App {
    app.schedule(
        zapline::tasks::Schedule::every(SWEEP_EVERY).name("soat-retention"),
        |res: Resources| async move { sweep_due(&res).await.map(|_| ()) },
    )
}

/// How long the shop keeps each kind of data, and what a sweep would
/// remove now (`privacy:read`).
#[get("/api/retention", tag = "privacy")]
async fn get_retention(
    db: Db,
    tenant: Tenant,
    map: State<RetentionMap>,
) -> Result<Json<RetentionView>> {
    let mut tx = tenant.begin_for(&db, &PRIVACY_READ).await?;
    let out = view(&mut tx, &tenant, &map).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Set how long each kind of data is kept, in days (`null` keeps it;
/// `reset` sets rules back to their default), never below a rule's
/// `min_days` (`privacy:settings`). Publishes `retention.updated`.
#[put("/api/retention", tag = "privacy")]
async fn put_retention(
    db: Db,
    tenant: Tenant,
    map: State<RetentionMap>,
    Json(input): Json<RetentionInput>,
) -> Result<Json<RetentionView>> {
    let all = rules(&map);
    let bad = |m: String| Err(Error::unprocessable(m).with_code("invalid_retention"));
    let mut tx = tenant.begin_for(&db, &PRIVACY_SETTINGS).await?;
    let mut days = stored(&mut tx, &tenant).await?.days;
    for key in &input.reset {
        if !all.iter().any(|r| r.key == key) {
            return bad(format!("{key} is not a retention rule"));
        }
        days.remove(key);
    }
    for (key, d) in &input.days {
        let Some(rule) = all.iter().find(|r| r.key == key) else {
            return bad(format!("{key} is not a retention rule"));
        };
        if let Some(d) = d {
            if *d < rule.min_days || *d > MAX_DAYS {
                return bad(format!(
                    "{key} is kept {} to {MAX_DAYS} days, or forever (null)",
                    rule.min_days
                ));
            }
        }
        days.insert(key.clone(), json!(d));
    }
    sqlx::query(
        "INSERT INTO retention_settings (tenant_id, days) VALUES ($1, $2) \
         ON CONFLICT (tenant_id) DO UPDATE SET days = EXCLUDED.days, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(Value::Object(days.clone()))
    .execute(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "retention.updated", &json!({ "days": days }))
        .await?;
    let out = view(&mut tx, &tenant, &map).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Remove now what the shop's rules say is past its time (`privacy:erase`,
/// since it can erase inactive customers). Publishes `retention.swept`.
#[post("/api/retention/sweep", tag = "privacy")]
async fn sweep_now(
    db: Db,
    tenant: Tenant,
    map: State<RetentionMap>,
    personal: State<PersonalDataMap>,
) -> Result<Json<SweepReport>> {
    let mut tx = tenant.begin_for(&db, &PRIVACY_ERASE).await?;
    let report = sweep(&mut tx, &tenant, &map, &personal, true).await?;
    tx.commit().await?;
    Ok(Json(report))
}

pub(crate) fn mount(app: App) -> App {
    schedule(app.mount((get_retention, put_retention, sweep_now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shop_choice_wins_over_the_default() {
        let stored = json!({ "carts": 30, "events": null });
        let stored = stored.as_object().unwrap();
        assert_eq!(days_of(stored, "carts", Some(90)), Some(30));
        assert_eq!(days_of(stored, "events", Some(400)), None);
        assert_eq!(days_of(stored, "emails", Some(730)), Some(730));
        assert_eq!(days_of(stored, "emails", None), None);
    }
}
