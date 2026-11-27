//! What personal data a module keeps, so a shop can hand a customer their
//! data (GDPR art. 15 and 20) and erase it (art. 17) without knowing every
//! module's tables.

/// One table of a module that holds data about a customer.
///
/// `matches` is an SQL condition on the table's rows, in which `$2` is the
/// customer's id and `$3` their email (lowercase); the tenant filter
/// (`tenant_id = $1`) is added. Every name here is written by the module,
/// never taken from a request.
///
/// ```
/// use soat_module::{Erase, PersonalData};
/// const WISHLIST: PersonalData = PersonalData {
///     table: "wishlist_items",
///     matches: "customer_id = $2",
///     export: &["product_id", "created_at"],
///     erase: Erase::Delete,
/// };
/// assert_eq!(WISHLIST.erase, Erase::Delete);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonalData {
    pub table: &'static str,
    pub matches: &'static str,
    /// Columns given in an export; empty for none (e.g. session tokens).
    pub export: &'static [&'static str],
    pub erase: Erase,
}

/// What erasing a customer does to a table's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Erase {
    /// The rows are deleted.
    Delete,
    /// The rows stay, with these columns set to these SQL values (e.g.
    /// `("email", "''")`): for records kept for others, like a discount's
    /// use count.
    Anonymize(&'static [(&'static str, &'static str)]),
    /// The rows stay as they are, for this legal reason (e.g. invoices,
    /// kept for tax law).
    Keep(&'static str),
}

/// Every module's personal data, in module order, available to handlers
/// as `State<PersonalDataMap>`.
#[derive(Clone, Debug, Default)]
pub struct PersonalDataMap(pub std::sync::Arc<Vec<(&'static str, PersonalData)>>);

impl PersonalDataMap {
    /// `(module, declaration)` pairs, in module order.
    pub fn all(&self) -> &[(&'static str, PersonalData)] {
        &self.0
    }

    /// Whether some module declares `table`.
    pub fn covers(&self, table: &str) -> bool {
        self.0.iter().any(|(_, d)| d.table == table)
    }
}

/// How long a shop keeps a table's old rows before a daily sweep deletes
/// them (GDPR art. 5(1)(e): no longer than needed). Each shop can change
/// the days, within `min_days`, or keep the rows forever.
///
/// `expired` is an SQL condition true for rows older than `$2` days (an
/// `integer`, e.g. `updated_at < now() - make_interval(days => $2)`); the
/// tenant filter (`tenant_id = $1`) is added. Every
/// name here is written by the module, never taken from a request.
///
/// ```
/// use soat_module::Retention;
/// const CARTS: Retention = Retention {
///     key: "abandoned_carts",
///     label: "Carts never checked out",
///     table: "carts",
///     expired: "status = 'open' AND updated_at < now() - make_interval(days => $2)",
///     default_days: Some(90),
///     min_days: 7,
/// };
/// assert_eq!(CARTS.default_days, Some(90));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// Unique across modules, lowercase with underscores.
    pub key: &'static str,
    /// What the rows are, for staff.
    pub label: &'static str,
    pub table: &'static str,
    pub expired: &'static str,
    /// `None`: kept until the shop sets a time.
    pub default_days: Option<i32>,
    /// The shortest time a shop may choose.
    pub min_days: i32,
}

/// Every module's retention rules, in module order, available to handlers
/// as `State<RetentionMap>`.
#[derive(Clone, Debug, Default)]
pub struct RetentionMap(pub std::sync::Arc<Vec<(&'static str, Retention)>>);

impl RetentionMap {
    /// `(module, rule)` pairs, in module order.
    pub fn all(&self) -> &[(&'static str, Retention)] {
        &self.0
    }

    /// The rule called `key`.
    pub fn get(&self, key: &str) -> Option<&Retention> {
        self.0.iter().map(|(_, r)| r).find(|r| r.key == key)
    }
}
