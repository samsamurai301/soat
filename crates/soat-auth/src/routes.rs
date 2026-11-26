//! Setup, login, logout and "who am I".

use zapline::auth::password::{hash_token, new_token, verify, verify_missing};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{create_user, normalize_email, Staff, SESSION_DAYS};

/// Email and password, for login.
#[model]
pub struct Credentials {
    #[validate(email)]
    pub email: String,
    #[validate(length(min = 1, max = 1024))]
    pub password: String,
}

/// The first platform admin, created once on a fresh install.
#[model]
pub struct Setup {
    /// From `SOAT_SETUP_TOKEN` or the startup log.
    pub setup_token: String,
    #[validate(email)]
    pub email: String,
    /// At least 12 characters.
    #[validate(length(min = 12, max = 1024))]
    pub password: String,
}

/// A user as the API shows them.
#[model]
pub struct UserView {
    pub id: String,
    pub email: String,
    pub is_platform_admin: bool,
}

/// A new login session. Send `token` as `Authorization: Bearer <token>`.
#[model]
pub struct Session {
    pub token: String,
    pub user: UserView,
}

#[derive(Clone)]
struct SetupToken(String);

pub(crate) fn mount(app: App, setup_token: String) -> App {
    app.state(SetupToken(setup_token))
        .mount((setup, login, logout, me))
}

async fn start_session(conn: &mut sqlx::PgConnection, user: UserView) -> Result<Session> {
    let token = new_token()?;
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at) \
         VALUES ($1, $2, now() + make_interval(days => $3))",
    )
    .bind(hash_token(&token))
    .bind(&user.id)
    .bind(SESSION_DAYS)
    .execute(&mut *conn)
    .await?;
    Ok(Session { token, user })
}

/// Create the first platform admin on a fresh install and sign them in.
/// 403 with a wrong setup token, 409 once any user exists.
#[post("/api/setup", tag = "auth", status = 201)]
async fn setup(
    db: Db,
    expected: State<SetupToken>,
    Valid(Json(s)): Valid<Json<Setup>>,
) -> Result<(StatusCode, Json<Session>)> {
    if !zapline::auth::constant_time_eq(&s.setup_token, &expected.0 .0) {
        return Err(Error::forbidden().with_code("invalid_setup_token"));
    }
    let mut tx = db.begin().await?;
    // One setup at a time; the lock is released at commit.
    sqlx::query("LOCK TABLE users IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let (users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    if users > 0 {
        return Err(Error::conflict("this install is already set up").with_code("already_set_up"));
    }
    let id = create_user(&mut tx, &s.email, &s.password, true).await?;
    let user = UserView {
        id: id.to_string(),
        email: normalize_email(&s.email),
        is_platform_admin: true,
    };
    let session = start_session(&mut tx, user).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(session)))
}

/// Sign in with email and password. Wrong email or password is the same 401.
#[post("/api/auth/login", tag = "auth")]
async fn login(db: Db, Valid(Json(c)): Valid<Json<Credentials>>) -> Result<Json<Session>> {
    let row: Option<(String, String, String, bool)> = sqlx::query_as(
        "SELECT id, email, password_hash, is_platform_admin FROM users WHERE email = $1",
    )
    .bind(normalize_email(&c.email))
    .fetch_optional(&*db)
    .await?;
    let Some((id, email, hash, is_platform_admin)) = row else {
        // Same work as a real check, so timing does not reveal the email.
        verify_missing(c.password).await?;
        return Err(Error::unauthorized().with_code("invalid_credentials"));
    };
    if !verify(c.password, hash).await? {
        return Err(Error::unauthorized().with_code("invalid_credentials"));
    }
    let mut conn = db.acquire().await?;
    let user = UserView {
        id,
        email,
        is_platform_admin,
    };
    Ok(Json(start_session(&mut conn, user).await?))
}

/// End the current session.
#[post("/api/auth/logout", tag = "auth", status = 204)]
async fn logout(db: Db, staff: Staff) -> Result<StatusCode> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(&staff.token_hash)
        .execute(&*db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The signed-in user.
#[get("/api/auth/me", tag = "auth")]
async fn me(staff: Staff) -> Result<Json<UserView>> {
    Ok(Json(UserView {
        id: staff.user_id.to_string(),
        email: staff.email,
        is_platform_admin: staff.is_platform_admin,
    }))
}
