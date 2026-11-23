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

mod migrations;

use std::collections::HashSet;
use std::fmt;

use zapline::embed::Dir;
use zapline::prelude::App;

pub use migrations::merge_migrations;

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

    /// Mounts the module's routes and plugins.
    fn mount(&self, app: App) -> App;
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
        self.migrations().map(|_| ())
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

    /// Mounts every module on `app`, in order.
    pub fn install(&self, app: App) -> App {
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

    #[test]
    fn a_module_cannot_depend_on_itself() {
        let selfish = Modules::new().with(Stub("loop", &["loop"]));
        assert!(selfish.validate().is_err());
    }
}
