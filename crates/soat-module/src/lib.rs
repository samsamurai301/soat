//! How SOAT is cut into modules.
//!
//! Every domain (tenancy, catalog, cart, orders...) is its own crate that
//! exports one value implementing [`Module`]: its name, the modules it needs,
//! its SQL migrations and its routes. The `soat` binary only lists modules in
//! a [`Modules`] set, which checks the set is consistent and wires it into a
//! Zapline [`App`].
//!
//! ```
//! use soat_module::{Module, Modules};
//! use zapline::prelude::*;
//!
//! struct Hello;
//!
//! impl Module for Hello {
//!     fn name(&self) -> &'static str {
//!         "hello"
//!     }
//!     fn mount(&self, app: App) -> App {
//!         app
//!     }
//! }
//!
//! let modules = Modules::new().with(Hello);
//! assert_eq!(modules.names(), ["hello"]);
//! ```

pub mod events;
mod migrations;
pub mod privacy;

use std::collections::HashSet;
use std::fmt;

use soat_core::{is_permission_name, Permission};
use zapline::embed::Dir;
use zapline::prelude::App;

pub use events::{Event, HandlerFuture, Subscriber, Subscribers};
pub use migrations::merge_migrations;
pub use privacy::{Erase, PersonalData, PersonalDataMap, Retention, RetentionMap};

/// One SOAT module.
///
/// A module never imports a sibling module's crate. When it needs another
/// module to exist (catalog needs tenancy), it names it in
/// [`depends_on`](Module::depends_on) and the set checks the order.
pub trait Module: Send + Sync + 'static {
    /// A short, unique, lowercase name such as `catalog`.
    fn name(&self) -> &'static str;

    /// Modules that must be listed before this one.
    fn depends_on(&self) -> &'static [&'static str] {
        &[]
    }

    /// The module's SQL migrations, usually `zapline::embed!("migrations")`.
    ///
    /// Versions are timestamps (`20261123000002_create_products.up.sql`) and
    /// must be unique across all modules, because they share one migrations
    /// table.
    fn migrations(&self) -> Option<Dir> {
        None
    }

    /// The permissions this module checks, e.g. `products:write`. Roles,
    /// API keys and AI tools are all scoped by these names.
    fn permissions(&self) -> &'static [Permission] {
        &[]
    }

    /// The tables in which this module keeps data about customers, for
    /// exporting and erasing it.
    fn personal_data(&self) -> &'static [PersonalData] {
        &[]
    }

    /// How long old rows of this module's tables are kept.
    fn retention(&self) -> &'static [Retention] {
        &[]
    }

    /// Subscribers that react to domain events.
    fn subscribers(&self) -> Vec<std::sync::Arc<dyn Subscriber>> {
        Vec::new()
    }

    /// Mounts the module's routes and plugins.
    fn mount(&self, app: App) -> App;
}

/// Every permission of an app's modules, available to handlers as
/// `State<Permissions>`.
#[derive(Debug, Clone, Default)]
pub struct Permissions(pub std::sync::Arc<Vec<Permission>>);

impl Permissions {
    pub fn all(&self) -> &[Permission] {
        &self.0
    }

    pub fn get(&self, name: &str) -> Option<&Permission> {
        self.0.iter().find(|p| p.name == name)
    }

    /// The names the built-in `staff` role grants.
    pub fn staff_defaults(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.0.iter().filter(|p| p.staff_default).map(|p| p.name)
    }
}

/// Why a set of modules was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleError {
    /// Two modules share a name.
    DuplicateName(&'static str),
    /// A module depends on one that is not listed before it.
    MissingDependency {
        module: &'static str,
        needs: &'static str,
    },
    /// Two modules ship a migration with the same version.
    DuplicateMigration {
        version: String,
        first: &'static str,
        second: &'static str,
    },
    /// A migration directory could not be read.
    Unreadable { module: &'static str, error: String },
    /// A permission name is malformed or declared twice.
    BadPermission {
        module: &'static str,
        permission: &'static str,
    },
    /// A retention rule's key is malformed or declared twice, or its days
    /// are below its minimum.
    BadRetention {
        module: &'static str,
        key: &'static str,
    },
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName(name) => write!(f, "module `{name}` is listed twice"),
            Self::MissingDependency { module, needs } => write!(
                f,
                "module `{module}` needs `{needs}`, which must be listed before it"
            ),
            Self::DuplicateMigration {
                version,
                first,
                second,
            } => write!(
                f,
                "migration version {version} is used by both `{first}` and `{second}`"
            ),
            Self::Unreadable { module, error } => {
                write!(f, "cannot read the migrations of `{module}`: {error}")
            }
            Self::BadPermission { module, permission } => write!(
                f,
                "module `{module}` declares `{permission}`, which is malformed \
                 (use resource:action) or already declared"
            ),
            Self::BadRetention { module, key } => write!(
                f,
                "module `{module}` declares the retention rule `{key}`, which is \
                 malformed (lowercase_with_underscores, default at least the minimum, \
                 minimum at least a day) or already declared"
            ),
        }
    }
}

impl std::error::Error for ModuleError {}

/// An ordered set of modules.
#[derive(Default)]
pub struct Modules(Vec<Box<dyn Module>>);

impl Modules {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a module after the ones already listed.
    #[must_use]
    pub fn with(mut self, module: impl Module) -> Self {
        self.0.push(Box::new(module));
        self
    }

    /// The module names, in order.
    pub fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(|m| m.name()).collect()
    }

    /// Checks names are unique, each dependency is listed earlier and no
    /// two modules share a migration version.
    pub fn validate(&self) -> Result<(), ModuleError> {
        let mut seen = HashSet::new();
        for module in &self.0 {
            for needs in module.depends_on() {
                if !seen.contains(needs) {
                    return Err(ModuleError::MissingDependency {
                        module: module.name(),
                        needs,
                    });
                }
            }
            if !seen.insert(module.name()) {
                return Err(ModuleError::DuplicateName(module.name()));
            }
        }
        let mut names = HashSet::new();
        for module in &self.0 {
            for p in module.permissions() {
                if !is_permission_name(p.name) || !names.insert(p.name) {
                    return Err(ModuleError::BadPermission {
                        module: module.name(),
                        permission: p.name,
                    });
                }
            }
        }
        let mut keys = HashSet::new();
        for module in &self.0 {
            for r in module.retention() {
                let well_formed = !r.key.is_empty()
                    && r.key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    && r.min_days >= 1
                    && r.default_days.is_none_or(|d| d >= r.min_days);
                if !well_formed || !keys.insert(r.key) {
                    return Err(ModuleError::BadRetention {
                        module: module.name(),
                        key: r.key,
                    });
                }
            }
        }
        let mut subscribers = HashSet::new();
        for module in &self.0 {
            for sub in module.subscribers() {
                if !subscribers.insert(sub.name()) {
                    return Err(ModuleError::DuplicateName(sub.name()));
                }
            }
        }
        self.migrations().map(|_| ())
    }

    /// Every module's permissions, in module order.
    pub fn permissions(&self) -> Permissions {
        let all = self
            .0
            .iter()
            .flat_map(|m| m.permissions().iter().copied())
            .collect();
        Permissions(std::sync::Arc::new(all))
    }

    /// Every module's personal data declarations, in module order.
    pub fn personal_data(&self) -> PersonalDataMap {
        PersonalDataMap(std::sync::Arc::new(
            self.0
                .iter()
                .flat_map(|m| m.personal_data().iter().map(|d| (m.name(), *d)))
                .collect(),
        ))
    }

    /// Every module's retention rules, in module order.
    pub fn retention(&self) -> RetentionMap {
        RetentionMap(std::sync::Arc::new(
            self.0
                .iter()
                .flat_map(|m| m.retention().iter().map(|r| (m.name(), *r)))
                .collect(),
        ))
    }

    /// All modules' migrations as one directory, for `Database::migrations`.
    pub fn migrations(&self) -> Result<Dir, ModuleError> {
        let dirs: Vec<_> = self
            .0
            .iter()
            .filter_map(|m| m.migrations().map(|dir| (m.name(), dir)))
            .collect();
        merge_migrations(&dirs)
    }

    /// Validates the set, runs its merged migrations on `db` and mounts
    /// every module on `app`.
    ///
    /// # Panics
    ///
    /// When the set is inconsistent: a build mistake that tests catch.
    pub fn build(&self, app: App, db: zapline::db::Database) -> App {
        self.validate()
            .unwrap_or_else(|e| panic!("invalid module list: {e}"));
        let migrations = self
            .migrations()
            .unwrap_or_else(|e| panic!("invalid module list: {e}"));
        self.install(app.database(db.migrations(migrations)))
    }

    /// Every module's event subscribers, in module order.
    pub fn subscribers(&self) -> Subscribers {
        Subscribers(std::sync::Arc::new(
            self.0.iter().flat_map(|m| m.subscribers()).collect(),
        ))
    }

    /// Mounts every module on `app`, in order, and makes the permission and
    /// subscriber lists available as `State<Permissions>`,
    /// `State<Subscribers>`, `State<PersonalDataMap>` and `State<RetentionMap>`.
    pub fn install(&self, app: App) -> App {
        let app = app
            .state(self.permissions())
            .state(self.subscribers())
            .state(self.personal_data())
            .state(self.retention());
        self.0.iter().fold(app, |app, module| module.mount(app))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub(&'static str, &'static [&'static str]);

    impl Module for Stub {
        fn name(&self) -> &'static str {
            self.0
        }
        fn depends_on(&self) -> &'static [&'static str] {
            self.1
        }
        fn mount(&self, app: App) -> App {
            app
        }
    }

    #[test]
    fn dependencies_must_come_first() {
        let ok = Modules::new()
            .with(Stub("tenancy", &[]))
            .with(Stub("catalog", &["tenancy"]));
        assert_eq!(ok.validate(), Ok(()));

        let wrong_order = Modules::new()
            .with(Stub("catalog", &["tenancy"]))
            .with(Stub("tenancy", &[]));
        assert_eq!(
            wrong_order.validate(),
            Err(ModuleError::MissingDependency {
                module: "catalog",
                needs: "tenancy"
            })
        );
    }

    #[test]
    fn names_are_unique() {
        let twice = Modules::new()
            .with(Stub("catalog", &[]))
            .with(Stub("catalog", &[]));
        assert_eq!(twice.validate(), Err(ModuleError::DuplicateName("catalog")));
    }

    struct Perms(&'static str, &'static [Permission]);

    impl Module for Perms {
        fn name(&self) -> &'static str {
            self.0
        }
        fn permissions(&self) -> &'static [Permission] {
            self.1
        }
        fn mount(&self, app: App) -> App {
            app
        }
    }

    #[test]
    fn permissions_are_well_formed_and_unique() {
        const READ: Permission = Permission::new("products:read", "See products").for_staff();
        const BAD: Permission = Permission::new("Products", "Nope");
        let ok = Modules::new().with(Perms("catalog", &[READ]));
        assert_eq!(ok.validate(), Ok(()));
        assert_eq!(
            ok.permissions().staff_defaults().collect::<Vec<_>>(),
            ["products:read"]
        );

        let twice = Modules::new()
            .with(Perms("a", &[READ]))
            .with(Perms("b", &[READ]));
        assert!(twice.validate().is_err());
        assert!(Modules::new().with(Perms("c", &[BAD])).validate().is_err());
    }

    #[test]
    fn a_module_cannot_depend_on_itself() {
        let selfish = Modules::new().with(Stub("loop", &["loop"]));
        assert!(selfish.validate().is_err());
    }
}
