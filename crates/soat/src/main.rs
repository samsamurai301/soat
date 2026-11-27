//! `soat`: the server binary, and the `soat app` and `soat login` commands
//! for app developers (`soat help`). Everything else lives in the library
//! crates, so tests build the very same `App` in-process.

use std::io::IsTerminal;

use zapline::db::Database;
use zapline::telemetry::{Telemetry, ENV_LOG_FORMAT};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if soat_cli::is_command(&args) {
        std::process::exit(soat_cli::run(&args));
    }
    // One JSON line per request (with its id) for log pipelines; readable
    // lines on a terminal. ZAPLINE_LOG_FORMAT and RUST_LOG override.
    let logs = Telemetry::new();
    if std::env::var_os(ENV_LOG_FORMAT).is_none() && !std::io::stdout().is_terminal() {
        logs.json().init();
    } else {
        logs.init();
    }
    let addr = std::env::var("ADDR").unwrap_or_else(|_| {
        let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
        format!("0.0.0.0:{port}")
    });
    // DATABASE_URL, or a local Postgres database named `soat`.
    let db = Database::from_env_or("postgres://localhost/soat");
    let settings = soat::Settings::from_env().unwrap_or_else(|e| {
        eprintln!("soat: {e}");
        std::process::exit(1);
    });
    soat::app(db, settings).run(addr)
}
