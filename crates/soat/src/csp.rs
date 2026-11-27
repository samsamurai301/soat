//! The Content Security Policy of every page: scripts run only when the
//! page's own templates wrote them (`soat_core::csp`), so a script that
//! slipped into content does not run.

use zapline::http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE};
use zapline::http::HeaderValue;
use zapline::prelude::*;
use zapline::Body;

/// Pages left without a policy: the API docs (they load their viewer from
/// a CDN) and the development mail inbox.
fn exempt(path: &str) -> bool {
    path == "/docs" || path.starts_with("/docs/") || path.starts_with("/_dev/")
}

/// The policy of a page under `path` whose templates' scripts and styles
/// carry `nonce`.
///
/// The admin loads nothing from elsewhere but images. The storefront also
/// runs what its theme, tracking (after consent) and apps load, so the
/// scripts it trusts may load others (`'strict-dynamic'`), and images,
/// styles, fonts and requests may come from any HTTPS origin. `https:` and
/// `'unsafe-inline'` only matter to browsers that predate nonces: newer
/// ones ignore them next to a nonce.
pub fn policy(path: &str, nonce: &str) -> String {
    if path == "/admin" || path.starts_with("/admin/") {
        format!(
            "default-src 'self'; script-src 'nonce-{nonce}' 'strict-dynamic'; \
             style-src 'self' 'nonce-{nonce}'; img-src 'self' data: blob: https:; \
             object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'"
        )
    } else {
        format!(
            "default-src 'self'; \
             script-src 'nonce-{nonce}' 'strict-dynamic' https: 'unsafe-inline'; \
             style-src 'self' 'nonce-{nonce}' https:; img-src 'self' data: https:; \
             font-src 'self' data: https:; connect-src 'self' https:; \
             frame-src 'self' https:; media-src 'self' https:; \
             object-src 'none'; base-uri 'self'; frame-ancestors 'none'"
        )
    }
}

/// Every HTML response: a fresh nonce in place of the templates'
/// placeholder, and the policy naming it. A response that already has a
/// policy keeps it.
pub(crate) fn wrap(app: App) -> App {
    app.wrap(|req: Request, next: Next| {
        let path = req.path().to_owned();
        async move {
            let mut res = next.run(req).await;
            let html = res
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.starts_with("text/html"));
            if !html || exempt(&path) || res.headers().contains_key(CONTENT_SECURITY_POLICY) {
                return res;
            }
            let nonce = soat_core::csp::nonce();
            if let Body::Full(Some(bytes)) = res.body() {
                if let Some(filled) = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|html| soat_core::csp::fill(html, &nonce))
                {
                    res.headers_mut()
                        .remove(zapline::http::header::CONTENT_LENGTH);
                    *res.body_mut() = Body::from(filled);
                }
            }
            if let Ok(v) = HeaderValue::from_str(&policy(&path, &nonce)) {
                res.headers_mut().insert(CONTENT_SECURITY_POLICY, v);
            }
            res
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_admin_and_the_storefront_have_their_own_policy() {
        let admin = policy("/admin/acme/orders", "n1");
        assert!(admin.contains("script-src 'nonce-n1' 'strict-dynamic';"));
        assert!(!admin.contains("unsafe-inline"));
        let shop = policy("/products/shoe", "n2");
        assert!(shop.contains("script-src 'nonce-n2' 'strict-dynamic'"));
        assert!(shop.contains("frame-ancestors 'none'"));
        assert!(exempt("/docs") && exempt("/_dev/mail") && !exempt("/documents"));
    }
}
