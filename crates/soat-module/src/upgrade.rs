//! Migrating at startup, and refusing a database this build is too old for.

use zapline::db::sqlx::PgPool;
use zapline::db::{MigrationInfo, MigrationState, Migrations};
use zapline::embed::Dir;
use zapline::{Error, Resources, Result};

/// Applies the pending migrations in `dir` to the app's pool, unless the
/// database holds migrations `dir` does not have: then it was upgraded by
/// a newer SOAT (or a build with more modules), and this build could
/// misread its tables, so startup stops with what to do.
///
/// # Errors
///
/// For such a database, or when a migration fails.
pub async fn migrate(res: Resources, dir: Dir) -> Result<()> {
    let pool: PgPool = res.require()?;
    let migrations = Migrations::open(dir).await?;
    let status = migrations.status(&pool).await?;
    refuse_unknown(&status)?;
    migrations.up(&pool).await?;
    Ok(())
}

/// An error naming the applied migrations this build does not know.
///
/// # Errors
///
/// When there are any.
pub fn refuse_unknown(status: &[MigrationInfo]) -> Result<()> {
    let unknown: Vec<String> = status
        .iter()
        .filter(|m| m.state == MigrationState::Missing)
        .map(|m| format!("{} {}", m.version, m.name))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(Error::internal(format!(
        "the database is newer than this build of SOAT: it has migrations this build \
         does not know ({}). It was upgraded by a newer SOAT, or by a build with more \
         features. Start that build again, or restore the backup taken before the \
         upgrade (docs/backups.md).",
        unknown.join(", ")
    )))
}
