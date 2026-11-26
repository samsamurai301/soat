//! Who is calling.
//!
//! - **Users**: one account per person, identified by email. A user is either
//!   a platform admin (runs this SOAT install and creates shops) or staff of
//!   one or more shops (memberships live in `soat-tenancy`).
//! - **Sessions**: `POST /api/auth/login` returns a random bearer token; only
//!   its SHA-256 is stored. Send it as `Authorization: Bearer <token>`.
//!   Sessions last 30 days and end at logout.
//! - **First run**: with no users yet, `POST /api/setup` creates the first
//!   platform admin. It needs the setup token, from `SOAT_SETUP_TOKEN` or
//!   printed to the log at startup, so nobody else can claim a fresh install.
//!
//! Handlers take the [`Staff`] extractor to require a signed-in user.

mod routes;
mod staff;

pub use routes::{Credentials, Session, Setup, UserView};
pub use staff::{Staff, UserId};

use soat_module::Module;
use zapline::embed::Dir;
use zapline::prelude::App;

/// How long a login session lasts.
pub const SESSION_DAYS: i32 = 30;

/// The auth module. It depends on nothing, so it is listed first.
#[derive(Clone)]
pub struct Auth {
    setup_token: String,
}

impl Auth {
    /// Uses `SOAT_SETUP_TOKEN`, or a random token printed at startup.
    pub fn from_env() -> Self {
        match std::env::var("SOAT_SETUP_TOKEN") {
            Ok(token) if !token.is_empty() => Self::with_setup_token(token),
            _ => {
                let token = zapline::auth::password::new_token()
                    .expect("the OS random source is available");
                eprintln!(
                    "SOAT setup token (only needed once, to create the first admin \
                     with POST /api/setup): {token}"
                );
                Self::with_setup_token(token)
            }
        }
    }

    /// A fixed setup token, for tests and scripted installs.
    pub fn with_setup_token(token: impl Into<String>) -> Self {
        Self {
            setup_token: token.into(),
        }
    }
}

impl Module for Auth {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn mount(&self, app: App) -> App {
        routes::mount(app, self.setup_token.clone())
    }
}

/// Normalizes an email for storage and lookup: trimmed and lowercased.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Creates a user with a hashed password and returns their id. A taken
/// email is a 409 (`email_taken`).
pub async fn create_user(
    conn: &mut zapline::db::sqlx::PgConnection,
    email: &str,
    password: &str,
    platform_admin: bool,
) -> zapline::Result<UserId> {
    use zapline::db::sqlx;
    let id = UserId::new();
    let hash = zapline::auth::password::hash(password).await?;
    let inserted = sqlx::query(
        "INSERT INTO users (id, email, password_hash, is_platform_admin) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (email) DO NOTHING",
    )
    .bind(id.as_str())
    .bind(normalize_email(email))
    .bind(hash)
    .bind(platform_admin)
    .execute(&mut *conn)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(
            zapline::Error::conflict("a user with this email exists").with_code("email_taken")
        );
    }
    Ok(id)
}
