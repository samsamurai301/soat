//! `soat`: the server binary. Everything else lives in the library crate, so
//! tests build the very same `App` in-process.

use zapline::db::Database;

fn main() -> std::io::Result<()> {
    let addr = std::env::var("ADDR").unwrap_or_else(|_| {
        let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
        format!("0.0.0.0:{port}")
    });
    // DATABASE_URL, or a local Postgres database named `soat`.
    let db = Database::from_env_or("postgres://localhost/soat");
    soat::app(db).run(addr)
}
