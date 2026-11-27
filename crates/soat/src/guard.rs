//! What guards every request before a module sees it: cross-site request
//! forgery checks on the admin's and the storefront's forms, and limits on
//! guessing (passwords, sign-in links, gift card codes).

use std::time::Duration;

use zapline::middleware::{RateLimit, SameOrigin, TrustedProxies};
use zapline::prelude::*;

/// Guessing attempts allowed per client and endpoint, per minute.
pub const GUESSES_PER_MINUTE: u32 = 20;

/// Where a password, a code or a link is checked, by path. Ids in a path
/// count as one endpoint.
fn guessable(method: &Method, path: &str) -> Option<&'static str> {
    if *method != Method::POST {
        return None;
    }
    Some(match path {
        "/admin/login" | "/api/auth/login" => "staff-login",
        "/admin/setup" | "/api/setup" => "setup",
        "/account/login" | "/api/storefront/account/login" => "login",
        "/account/register" | "/api/storefront/account/register" => "register",
        "/account/forgot" | "/account/access" | "/api/storefront/account/access" => "access",
        "/api/storefront/account/access/complete" => "access-complete",
        "/api/storefront/gift-cards/balance" => "gift-card",
        "/api/storefront/carts/recover" => "cart-recover",
        "/api/apps/token" => "app-token",
        "/unsubscribe" | "/api/storefront/unsubscribe" => "unsubscribe",
        _ => return None,
    })
}

/// Installs both guards; `TRUSTED_PROXIES` (comma-separated addresses or
/// networks) names the proxies whose `X-Forwarded-For` tells the client.
///
/// The admin and storefront set cookies and take form posts, so a browser
/// may only post to them from the same origin. The API takes tokens in
/// headers, never cookies, and answers other origins by design (headless
/// storefronts, apps), so it is exempt.
pub(crate) fn wrap(app: App) -> App {
    // A wrong TRUSTED_PROXIES would limit the wrong clients: refuse to start.
    let proxies = TrustedProxies::from_env().unwrap_or_else(|e| panic!("{e}"));
    let guesses = RateLimit::by_key(
        GUESSES_PER_MINUTE,
        Duration::from_secs(60),
        move |req: &Request| {
            let endpoint = guessable(req.method(), req.path())?;
            let client = proxies
                .client_ip(req)
                .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
            Some(format!("{endpoint}|{client}"))
        },
    );
    app.wrap(SameOrigin::new().exempt(["/api/"])).wrap(guesses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_posts_that_check_secrets_are_limited() {
        assert_eq!(
            guessable(&Method::POST, "/admin/login"),
            Some("staff-login")
        );
        assert_eq!(
            guessable(&Method::POST, "/api/storefront/gift-cards/balance"),
            Some("gift-card")
        );
        assert_eq!(guessable(&Method::GET, "/admin/login"), None);
        assert_eq!(guessable(&Method::POST, "/cart/add"), None);
    }
}
