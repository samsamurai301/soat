//! The `soat app` commands, run by the `soat` binary: start an app from a
//! template, register it on a SOAT server from its `soat.app.json`, and
//! keep it in step while developing.
//!
//! ```text
//! soat login https://shop.example.com you@example.com
//! soat app new reviews --name Reviews
//! cd reviews && soat app dev --shop acme      # registers, writes .env, prints the install link
//! node server.mjs
//! ```
//!
//! Every call goes through [`Api`], so tests drive the same code against
//! an in-process server.

use std::future::Future;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{json, Map, Value};
use soat_http::{Bytes, Full, Method, Request};

/// The app's description, kept in the app's own folder.
pub const MANIFEST: &str = "soat.app.json";
/// Which server the app is registered on and its id (no secrets).
pub const STATE: &str = ".soat/app.json";

/// What an [`Api`] call returns: the status and the JSON body.
pub type ApiFuture<'a> = Pin<Box<dyn Future<Output = Result<(u16, Value), String>> + Send + 'a>>;

/// A SOAT server's API, signed in as someone.
pub trait Api: Send + Sync {
    /// The server's address, e.g. `https://shop.example.com`.
    fn server(&self) -> &str;
    /// Calls `method path` with `body` as JSON.
    fn call<'a>(&'a self, method: &'a str, path: &'a str, body: Option<Value>) -> ApiFuture<'a>;
}

/// The API over HTTP.
pub struct HttpApi {
    pub server: String,
    /// A session token from `soat login`.
    pub token: Option<String>,
}

impl Api for HttpApi {
    fn server(&self) -> &str {
        &self.server
    }

    fn call<'a>(&'a self, method: &'a str, path: &'a str, body: Option<Value>) -> ApiFuture<'a> {
        Box::pin(async move {
            let method = Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
            let mut req = Request::builder()
                .method(method)
                .uri(format!("{}{path}", self.server))
                .header("accept", "application/json");
            if let Some(t) = &self.token {
                req = req.header("authorization", format!("Bearer {t}"));
            }
            let bytes = match body {
                Some(b) => {
                    req = req.header("content-type", "application/json");
                    Bytes::from(b.to_string())
                }
                None => Bytes::new(),
            };
            let req = req.body(Full::new(bytes)).map_err(|e| e.to_string())?;
            let answer = soat_http::send("SOAT", req)
                .await
                .map_err(|e| e.to_string())?;
            Ok((answer.status.as_u16(), answer.json()))
        })
    }
}

const TEMPLATE: &[(&str, &str)] = &[
    (MANIFEST, include_str!("../template/soat.app.json")),
    ("server.mjs", include_str!("../template/server.mjs")),
    ("package.json", include_str!("../template/package.json")),
    (".gitignore", include_str!("../template/gitignore")),
    ("README.md", include_str!("../template/README.md")),
];

/// `name` as a package name: lowercase letters, digits and `-`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() {
        "soat-app".to_owned()
    } else {
        out
    }
}

/// Writes a new app called `name` into `dir`, which must be new or empty.
/// Returns the files written.
pub fn new_app(dir: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.contains(['"', '\\', '<', '>']) {
        return Err("the name is 1 to 100 characters, without quotes or < >".into());
    }
    if dir.exists()
        && std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!("{} is not empty", dir.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let slug = slug(name);
    let mut written = Vec::new();
    for (file, text) in TEMPLATE {
        let path = dir.join(file);
        let text = text.replace("{{name}}", name).replace("{{slug}}", &slug);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// What [`push`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Pushed {
    pub client_id: String,
    /// Registered just now (and `.env` written), not updated.
    pub created: bool,
    /// The first redirect address, for the install link.
    pub redirect_uri: String,
}

/// The fields of the manifest `PATCH /api/apps/{id}` takes.
const FIELDS: &[&str] = &[
    "name",
    "redirect_uris",
    "permissions",
    "tagline",
    "description",
    "category",
    "app_url",
    "listed",
    "admin_links",
    "storefront_blocks",
];

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The server's complaint, for people.
fn complaint(status: u16, body: &Value) -> String {
    let said = body["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or("no reason given");
    match status {
        401 => "not signed in: run `soat login <server> <email>`".to_owned(),
        s => format!("the server said {s}: {said}"),
    }
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Registers the app in `dir` on the API's server the first time (writing
/// its id to `.soat/app.json` and its secret to `.env`), then brings the
/// registration in line with `soat.app.json`.
pub async fn push(api: &dyn Api, dir: &Path) -> Result<Pushed, String> {
    let manifest = read_json(&dir.join(MANIFEST))?;
    let Some(fields) = manifest.as_object() else {
        return Err(format!("{MANIFEST} is not a JSON object"));
    };
    let redirect_uri = manifest["redirect_uris"][0]
        .as_str()
        .ok_or_else(|| format!("{MANIFEST} has no redirect_uris"))?
        .to_owned();
    let state_path = dir.join(STATE);
    let known = read_json(&state_path)
        .ok()
        .filter(|s| s["server"] == api.server())
        .and_then(|s| s["client_id"].as_str().map(str::to_owned));
    let (client_id, created) = match known {
        Some(id) => (id, false),
        None => {
            let body = json!({
                "name": manifest["name"], "redirect_uris": manifest["redirect_uris"],
                "permissions": manifest["permissions"],
            });
            let (status, out) = api.call("POST", "/api/apps", Some(body)).await?;
            if status != 201 {
                return Err(complaint(status, &out));
            }
            let id = out["app"]["id"].as_str().unwrap_or_default().to_owned();
            let secret = out["client_secret"].as_str().unwrap_or_default();
            std::fs::create_dir_all(dir.join(".soat")).map_err(|e| e.to_string())?;
            let state = json!({ "server": api.server(), "client_id": id });
            std::fs::write(&state_path, format!("{state:#}\n")).map_err(|e| e.to_string())?;
            write_private(
                &dir.join(".env"),
                &format!(
                    "SOAT_URL={}\nSOAT_CLIENT_ID={id}\nSOAT_CLIENT_SECRET={secret}\n",
                    api.server()
                ),
            )?;
            (id, true)
        }
    };
    let mut change = Map::new();
    for f in FIELDS {
        match fields.get(*f) {
            Some(Value::Null) if *f == "app_url" => {
                change.insert((*f).to_owned(), json!(""));
            }
            Some(v) => {
                change.insert((*f).to_owned(), v.clone());
            }
            None => {}
        }
    }
    let path = format!("/api/apps/{client_id}");
    let (status, out) = api
        .call("PATCH", &path, Some(Value::Object(change)))
        .await?;
    match status {
        200 => Ok(Pushed {
            client_id,
            created,
            redirect_uri,
        }),
        404 => Err(format!(
            "{client_id} is not one of your apps on {}: delete {STATE} to register it again",
            api.server()
        )),
        s => Err(complaint(s, &out)),
    }
}

/// The address that installs the app in `shop`: SOAT's consent screen,
/// sending the owner back to `redirect_uri`.
pub fn install_link(server: &str, shop: &str, client_id: &str, redirect_uri: &str) -> String {
    let query =
        serde_urlencoded::to_string([("client_id", client_id), ("redirect_uri", redirect_uri)])
            .unwrap_or_default();
    format!("{server}/admin/{shop}/apps/authorize?{query}")
}

/// Signs in with email and password; returns the session token.
pub async fn login(api: &dyn Api, email: &str, password: &str) -> Result<String, String> {
    let (status, out) = api
        .call(
            "POST",
            "/api/auth/login",
            Some(json!({ "email": email, "password": password })),
        )
        .await?;
    match (status, out["token"].as_str()) {
        (200, Some(t)) => Ok(t.to_owned()),
        (401, _) => Err("wrong email or password".into()),
        (s, _) => Err(complaint(s, &out)),
    }
}

fn session_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("soat").join("session.json"))
}

/// The server and token: `SOAT_SERVER` and `SOAT_TOKEN`, else `soat login`'s.
fn signed_in() -> Result<HttpApi, String> {
    let saved = session_path().and_then(|p| read_json(&p).ok());
    let server = std::env::var("SOAT_SERVER")
        .ok()
        .or_else(|| saved.as_ref()?["server"].as_str().map(str::to_owned))
        .ok_or("no server: run `soat login <server> <email>`")?;
    let token = std::env::var("SOAT_TOKEN")
        .ok()
        .or_else(|| saved.as_ref()?["token"].as_str().map(str::to_owned));
    Ok(HttpApi {
        server: server.trim_end_matches('/').to_owned(),
        token,
    })
}

const HELP: &str = "\
soat                                  run the server
soat login <server> <email>           sign in (password from SOAT_PASSWORD or asked)
soat app new <dir> [--name <name>]    start an app from the template
soat app push [--dir <dir>]           register the app, or update it from soat.app.json
soat app dev --shop <shop> [--dir <dir>]
                                      push, then print the link that installs it in <shop>";

/// The value after `--flag`.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Whether `args` (without the program name) are a CLI command rather
/// than a server start.
pub fn is_command(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("app" | "login" | "help" | "--help" | "-h")
    )
}

/// Runs a command; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soat: {e}");
            return 1;
        }
    };
    match runtime.block_on(command(args)) {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("soat: {e}");
            1
        }
    }
}

async fn command(args: &[String]) -> Result<String, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let dir = PathBuf::from(flag(args, "--dir").unwrap_or_else(|| ".".into()));
    match words.as_slice() {
        ["login", server, email, ..] => {
            let api = HttpApi {
                server: server.trim_end_matches('/').to_owned(),
                token: None,
            };
            let password = match std::env::var("SOAT_PASSWORD") {
                Ok(p) => p,
                Err(_) => {
                    eprint!("Password for {email}: ");
                    let mut line = String::new();
                    std::io::stdin()
                        .lock()
                        .read_line(&mut line)
                        .map_err(|e| e.to_string())?;
                    line.trim_end_matches(['\r', '\n']).to_owned()
                }
            };
            let token = login(&api, email, &password).await?;
            let path = session_path().ok_or("no home folder to keep the session in")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            write_private(
                &path,
                &json!({ "server": api.server, "token": token }).to_string(),
            )?;
            Ok(format!("Signed in to {} as {email}.", api.server))
        }
        ["app", "new", target, ..] => {
            let name = flag(args, "--name").unwrap_or_else(|| (*target).to_owned());
            let files = new_app(Path::new(target), &name)?;
            Ok(format!(
                "Wrote {} files to {target}. Next: cd {target} && soat app dev --shop <shop>",
                files.len()
            ))
        }
        ["app", "push", ..] => {
            let api = signed_in()?;
            let pushed = push(&api, &dir).await?;
            Ok(if pushed.created {
                format!(
                    "Registered {} on {}; its secret is in .env.",
                    pushed.client_id, api.server
                )
            } else {
                format!("Updated {} on {}.", pushed.client_id, api.server)
            })
        }
        ["app", "dev", ..] => {
            let shop = flag(args, "--shop").ok_or("which shop? --shop <shop>")?;
            let api = signed_in()?;
            let pushed = push(&api, &dir).await?;
            Ok(format!(
                "{} {} on {}.\nStart the app (node server.mjs), then install it in {shop}:\n{}",
                if pushed.created {
                    "Registered"
                } else {
                    "Updated"
                },
                pushed.client_id,
                api.server,
                install_link(&api.server, &shop, &pushed.client_id, &pushed.redirect_uri)
            ))
        }
        _ => Ok(HELP.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_links() {
        assert_eq!(slug("Product Reviews!"), "product-reviews");
        assert_eq!(slug("***"), "soat-app");
        assert_eq!(
            install_link("http://s", "acme", "app_1", "http://localhost:3000/cb"),
            "http://s/admin/acme/apps/authorize?client_id=app_1&redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fcb"
        );
        assert!(is_command(&["app".into()]));
        assert!(!is_command(&[]));
    }

    #[test]
    fn flags_are_read() {
        let args: Vec<String> = ["app", "dev", "--shop", "acme"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(flag(&args, "--shop").as_deref(), Some("acme"));
        assert_eq!(flag(&args, "--dir"), None);
    }
}
