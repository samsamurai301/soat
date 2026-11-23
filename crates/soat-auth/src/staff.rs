//! The [`Staff`] extractor: the signed-in user behind a bearer token.

use soat_core::id_kind;
use zapline::auth::password::hash_token;
use zapline::auth::BearerToken;
use zapline::db::{sqlx, Db};
use zapline::extract::FromRequestParts;
use zapline::prelude::*;

id_kind!(UserId, UserKind, "user");

/// A signed-in user. Rejects a missing, unknown or expired token with 401.
#[derive(Debug, Clone)]
pub struct Staff {
    pub user_id: UserId,
    pub email: String,
    /// Runs this install: may create shops and act in any shop.
    pub is_platform_admin: bool,
    /// The SHA-256 of the session token, to end this session.
    pub(crate) token_hash: String,
}

impl FromRequestParts for Staff {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let token = BearerToken::from_request_parts(req).await?;
        let db = Db::from_request_parts(req).await?;
        let token_hash = hash_token(token.token());
        let row: Option<(String, String, bool)> = sqlx::query_as(
            "SELECT u.id, u.email, u.is_platform_admin FROM sessions s \
             JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = $1 AND s.expires_at > now()",
        )
        .bind(&token_hash)
        .fetch_optional(&*db)
        .await?;
        let Some((id, email, is_platform_admin)) = row else {
            return Err(Error::unauthorized().with_code("invalid_session"));
        };
        let user_id =
            UserId::parse(&id).map_err(|e| Error::internal(format!("stored user id: {e}")))?;
        Ok(Staff {
            user_id,
            email,
            is_platform_admin,
            token_hash,
        })
    }
}

impl Staff {
    /// 403 unless this user is a platform admin.
    pub fn require_platform_admin(&self) -> Result<()> {
        if self.is_platform_admin {
            Ok(())
        } else {
            Err(Error::forbidden().with_code("platform_admin_only"))
        }
    }
}
