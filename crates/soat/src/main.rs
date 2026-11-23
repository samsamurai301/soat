//! `soat`: the server binary. Everything else lives in the library crate, so
//! tests build the very same `App` in-process.

use zapline::db::Database;

fn main() -> std::io::Result<()> {
    let addr = std::env::var("ADDR").unwrap_or_else(|_| {
        let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
        format!("0.0.0.0:{port}")
    });
    // DATABASE_URL, or a local SQLite file so a fresh checkout runs with no setup.
    let db = Database::from_env_or("sqlite://soat.db?mode=rwc");
    soat::app(db).run(addr)
}
