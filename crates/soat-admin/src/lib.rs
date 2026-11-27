//! The admin: staff run their shops in the browser at `/admin`.
//!
//! Pages are rendered on the server and enhanced with htmx. Every page is a
//! client of the shop API, run in process with the signed-in staff
//! member's session: what the API lets them do, the admin lets them do, with
//! the same permissions, validation and events (the audit log names them).
//!
//! The session lives in an HttpOnly, `SameSite=Strict` cookie scoped to
//! `/admin`, and every form post must come from this site (Zapline's
//! [`SameOrigin`] check), so other sites cannot act for a signed-in user.

/// The page, or the response that replaces it (sign in, not found).
macro_rules! page {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(res) => return Ok(*res),
        }
    };
}

/// Like `page!`, in a function that answers a plain response.
macro_rules! page_or {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(res) => return *res,
        }
    };
}

mod analytics;
mod api;
mod apps;
mod assistant;
mod b2b;
mod blog;
mod builder;
mod campaigns;
mod collections;
mod content;
mod customers;
mod dashboard;
mod discounts;
mod feeds;
mod giftcards;
mod launch;
mod onboarding;
mod orders;
mod pages;
mod palette;
mod pos;
mod products;
mod reviews;
mod samples;
mod settings;
mod subscriptions;
mod theme;
mod transfer;
mod util;
mod variants;

use std::sync::Arc;

use serde_json::{json, Value};
use soat_module::Module;
use zapline::cookies::CookieJar;
use zapline::db::Db;
use zapline::middleware::{Next, SameOrigin};
use zapline::prelude::*;

use api::Api;
use pages::Pages;

/// The cookie holding a staff member's session token.
pub const ADMIN_COOKIE: &str = "soat_admin";

/// The admin module.
pub struct Admin {
    api: Api,
    pages: Arc<Pages>,
}

impl Admin {
    /// Serves the API that `build` mounts on a fresh app (the same modules
    /// as the server's, without background jobs) as admin pages. That app
    /// shares the server's database pool; it never listens on a socket.
    pub fn new(build: impl FnOnce(App) -> App) -> Self {
        Self {
            api: Api::new(build),
            pages: Arc::new(Pages::load()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Shared {
    api: Api,
    pages: Arc<Pages>,
}

impl Module for Admin {
    fn name(&self) -> &'static str {
        "admin"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy"]
    }

    fn mount(&self, app: App) -> App {
        app.state(Shared {
            api: self.api.clone(),
            pages: self.pages.clone(),
        })
        .wrap(guard)
        .mount((
            login_page,
            login,
            logout,
            shop_list,
            dashboard::home,
            palette::search,
            styles,
        ))
        .mount((
            onboarding::setup_page,
            onboarding::setup,
            onboarding::new_shop_page,
            onboarding::create_shop,
            onboarding::welcome,
            onboarding::welcome_step,
            launch::show,
            launch::start_page,
        ))
        .mount((
            assistant::index,
            assistant::show,
            assistant::start,
            assistant::send,
            assistant::act,
            builder::show,
            builder::draft,
            builder::apply,
        ))
        .mount((
            transfer::show,
            transfer::export_products,
            transfer::export_customers,
            transfer::import_products,
            transfer::import_customers,
            transfer::connect_shopify,
            transfer::sync_shopify,
        ))
        .mount((
            apps::list,
            apps::directory,
            apps::uninstall,
            apps::blocks,
            apps::authorize_page,
            apps::authorize,
        ))
        .mount((
            products::list,
            products::new_page,
            products::create,
            products::edit_page,
            products::update,
            products::delete,
            products::write,
            products::translate,
        ))
        .mount((
            collections::list,
            collections::new_page,
            collections::edit_page,
            collections::create,
            collections::update,
            collections::act,
        ))
        .mount((
            discounts::list,
            discounts::new_page,
            discounts::edit_page,
            discounts::create,
            discounts::update,
            discounts::delete,
        ))
        .mount((reviews::list, reviews::act))
        .mount((analytics::page,))
        .mount((
            campaigns::list,
            campaigns::create,
            campaigns::edit_page,
            campaigns::act,
        ))
        .mount((
            subscriptions::list,
            subscriptions::create_plan,
            subscriptions::toggle_plan,
            subscriptions::act,
        ))
        .mount((
            b2b::list,
            b2b::create,
            b2b::edit_page,
            b2b::act,
            b2b::quotes,
            b2b::quote,
            b2b::quote_act,
        ))
        .mount((feeds::list, feeds::create, feeds::act))
        .mount((
            pos::registers,
            pos::create_register,
            pos::register_act,
            pos::till,
            pos::till_act,
            pos::receipt,
        ))
        .mount((
            customers::adjust_points,
            giftcards::list,
            giftcards::loyalty,
            giftcards::new_page,
            giftcards::create,
            giftcards::edit_page,
            giftcards::act,
        ))
        .mount((
            variants::set_options,
            variants::add_variant,
            variants::add_all,
            variants::change_variant,
            variants::upload,
            variants::change_media,
            variants::upload_download,
            variants::remove_download,
        ))
        .mount((
            orders::list,
            orders::show,
            orders::ship,
            orders::cancel,
            orders::refund,
            orders::item_action,
        ))
        .mount((
            customers::list,
            customers::new_page,
            customers::create,
            customers::edit_page,
            customers::update,
            customers::delete,
        ))
        .mount((
            content::pages,
            content::new_page,
            content::create_page,
            content::edit_page,
            content::update_page,
            content::delete,
            content::menus,
            content::new_menu,
            content::edit_menu,
            content::create_menu,
            content::update_menu,
            content::redirects,
            content::add_redirect,
        ))
        .mount((
            blog::list,
            blog::new_page,
            blog::edit_page,
            blog::create,
            blog::update,
        ))
        .mount((
            theme::show,
            theme::save_settings,
            theme::edit_file,
            theme::save_file,
            theme::reset_file,
        ))
        .mount((
            settings::index,
            settings::show_area,
            settings::upload_plugin,
            settings::act,
            settings::act_on,
            settings::email_template,
            settings::save_email_template,
            settings::reset_email_template,
        ))
    }
}

/// Admin form posts must come from this site; admin pages are never
/// framed or cached.
async fn guard(req: Request, next: Next) -> Response {
    let admin = req.uri().path() == "/admin" || req.uri().path().starts_with("/admin/");
    if admin && !SameOrigin::new().allows(&req) {
        return Error::forbidden().with_code("cross_origin").into_response();
    }
    let mut res = next.run(req).await;
    if admin {
        let h = res.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        );
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    res
}

/// The signed-in staff member's session token.
fn token(headers: &HeaderMap) -> Option<String> {
    CookieJar::from_headers(headers)
        .get(ADMIN_COOKIE)
        .map(|c| c.value().to_owned())
        .filter(|t| !t.is_empty())
}

fn session_cookie(token: &str, max_age: i64, secure: bool) -> HeaderValue {
    let cookie = format!(
        "{ADMIN_COOKIE}={token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    );
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn to_login() -> Response {
    Redirect::to("/admin/login").into_response()
}

/// A page's shell data: the user's shops and the one being worked on.
pub(crate) struct Ctx {
    pub s: Shared,
    pub db: Db,
    pub conn: ConnInfo,
    pub token: String,
    /// The shop's id, e.g. `acme`.
    pub shop: String,
    pub data: serde_json::Map<String, Value>,
}

/// The page or a response that replaces it (sign in, not found).
/// A value, or the page to show instead (boxed: a response is large).
pub(crate) type Page<T> = std::result::Result<T, Box<Response>>;

impl Ctx {
    /// Opens shop `shop` for the signed-in staff member: their shops for
    /// the switcher, a sign-in redirect without a session, not found for a
    /// shop they do not work for.
    pub async fn open(
        s: Shared,
        db: Db,
        headers: &HeaderMap,
        conn: ConnInfo,
        shop: &str,
        section: &str,
    ) -> Page<Self> {
        let Some(token) = token(headers) else {
            return Err(Box::new(to_login()));
        };
        let (status, shops) = s
            .api
            .send(
                &db,
                Method::GET,
                "/api/tenants",
                None,
                Some(&token),
                None,
                conn,
            )
            .await;
        if status == 401 {
            return Err(Box::new(to_login()));
        }
        let list = shops.as_array().cloned().unwrap_or_default();
        let Some(current) = list.iter().find(|t| t["id"] == shop).cloned() else {
            return Err(Box::new(
                s.pages
                    .error(StatusCode::NOT_FOUND, "There is no such shop here."),
            ));
        };
        let mut data = serde_json::Map::new();
        data.insert("shop".into(), current);
        data.insert("shops".into(), Value::Array(list));
        data.insert("section".into(), json!({ section: true }));
        data.insert("base".into(), json!(format!("/admin/{shop}")));
        Ok(Self {
            s,
            db,
            conn,
            token,
            shop: shop.to_owned(),
            data,
        })
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_owned(), value);
    }

    /// Calls the shop API as the staff member: the status and the body.
    pub async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> (u16, Value) {
        self.s
            .api
            .send(
                &self.db,
                method,
                path,
                body,
                Some(&self.token),
                Some(&self.shop),
                self.conn,
            )
            .await
    }

    /// A read that must work: its body, or the page that says why not.
    pub async fn get(&self, path: &str) -> Page<Value> {
        let (status, body) = self.call(Method::GET, path, None).await;
        match status {
            200..=299 => Ok(body),
            401 => Err(Box::new(to_login())),
            _ => Err(Box::new(self.failed(status, &body))),
        }
    }

    /// The error page for an API answer.
    pub fn failed(&self, status: u16, body: &Value) -> Response {
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let message = match status {
            StatusCode::FORBIDDEN => "Your role does not allow this. Ask the shop's owner.",
            StatusCode::NOT_FOUND => "This is not here, or no longer.",
            _ => body["error"].as_str().unwrap_or("Something went wrong."),
        };
        self.s.pages.error(status, message)
    }

    /// The page `name` in the admin's shell.
    pub fn render(self, name: &str, status: StatusCode) -> Response {
        self.s.pages.render(name, &Value::Object(self.data), status)
    }
}

/// The sign-in form.
#[get("/admin/login", tag = "admin")]
async fn login_page(s: State<Shared>, headers: HeaderMap) -> Result<Response> {
    if token(&headers).is_some() {
        return Ok(Redirect::to("/admin").into_response());
    }
    Ok(s.pages
        .render("login", &json!({ "page_title": "Sign in" }), StatusCode::OK))
}

/// The sign-in form's fields.
#[model]
pub struct LoginForm {
    pub email: String,
    pub password: String,
}

/// Sign in (a form post): starts a staff session in a cookie.
#[post("/admin/login", tag = "admin")]
async fn login(
    s: State<Shared>,
    db: Db,
    conn: ConnInfo,
    Form(f): Form<LoginForm>,
) -> Result<Response> {
    let body = json!({ "email": f.email.trim(), "password": f.password });
    let (status, session) = s
        .api
        .send(
            &db,
            Method::POST,
            "/api/auth/login",
            Some(&body),
            None,
            None,
            conn,
        )
        .await;
    match (status, session["token"].as_str()) {
        (200, Some(token)) => {
            let mut res = Redirect::to("/admin").into_response();
            res.headers_mut().append(
                header::SET_COOKIE,
                session_cookie(token, 30 * 86_400, conn.secure),
            );
            Ok(res)
        }
        (200 | 401 | 422, _) => Ok(s.pages.render(
            "login",
            &json!({
                "page_title": "Sign in",
                "email": f.email,
                "error": "Wrong email or password.",
            }),
            StatusCode::UNAUTHORIZED,
        )),
        _ => Err(Error::internal("signing in failed")),
    }
}

/// Sign out (a form post): ends the session and drops its cookie.
#[post("/admin/logout", tag = "admin")]
async fn logout(s: State<Shared>, db: Db, headers: HeaderMap, conn: ConnInfo) -> Result<Response> {
    if let Some(t) = token(&headers) {
        // An expired session is signed out already.
        let _ = s
            .api
            .send(
                &db,
                Method::POST,
                "/api/auth/logout",
                None,
                Some(&t),
                None,
                conn,
            )
            .await;
    }
    let mut res = Redirect::to("/admin/login").into_response();
    res.headers_mut()
        .append(header::SET_COOKIE, session_cookie("", 0, conn.secure));
    Ok(res)
}

/// The shops the staff member works for; one shop opens directly.
#[get("/admin", tag = "admin")]
async fn shop_list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
) -> Result<Response> {
    let Some(token) = token(&headers) else {
        return Ok(to_login());
    };
    let (status, shops) = s
        .api
        .send(
            &db,
            Method::GET,
            "/api/tenants",
            None,
            Some(&token),
            None,
            conn,
        )
        .await;
    if status == 401 {
        let mut res = to_login();
        res.headers_mut()
            .append(header::SET_COOKIE, session_cookie("", 0, conn.secure));
        return Ok(res);
    }
    let list = shops.as_array().cloned().unwrap_or_default();
    if let [only] = list.as_slice() {
        if let Some(id) = only["id"].as_str() {
            return Ok(Redirect::to(&format!("/admin/{id}")).into_response());
        }
    }
    let (_, me) = s
        .api
        .send(
            &db,
            Method::GET,
            "/api/auth/me",
            None,
            Some(&token),
            None,
            conn,
        )
        .await;
    let admin = me["is_platform_admin"] == json!(true);
    if admin && list.is_empty() {
        return Ok(Redirect::to("/admin/new").into_response());
    }
    Ok(s.pages.render(
        "shops",
        &json!({ "page_title": "Your shops", "shops": list, "can_create": admin }),
        StatusCode::OK,
    ))
}

/// The admin's stylesheet.
#[get("/admin/admin.css", tag = "admin")]
async fn styles() -> Response {
    let mut res = pages::CSS.into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    res
}
