//! One module per file; `install` mounts them all.

pub mod catalog;
pub mod tenants;

use zapline::prelude::*;

pub fn install(mut app: App) -> App {
    app = app.plugin(tenants::mount);
    app = app.plugin(catalog::mount);
    app
}
