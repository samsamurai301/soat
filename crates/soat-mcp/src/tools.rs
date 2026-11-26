//! MCP tools from the OpenAPI spec: one per API operation, and the HTTP
//! request a tool call stands for.

use serde_json::{json, Map, Value};
use zapline::http::Method;

/// An API operation offered as a tool.
#[derive(Debug, Clone)]
pub struct Tool {
    /// The operation id, e.g. `create_product`.
    pub name: String,
    pub description: String,
    pub method: Method,
    /// The path pattern, e.g. `/api/products/{id}`.
    pub path: String,
    path_params: Vec<String>,
    query_params: Vec<String>,
    has_body: bool,
    /// JSON Schema of the arguments: path and query parameters by name, the
    /// request body as `body`.
    pub input_schema: Value,
}

impl Tool {
    /// Whether calling it changes something.
    pub fn writes(&self) -> bool {
        self.method != Method::GET && self.method != Method::HEAD
    }

    /// The tool as `tools/list` shows it.
    pub fn listing(&self) -> Value {
        json!({
            "name": self.name,
            "title": self.name.replace('_', " "),
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": {
                "readOnlyHint": !self.writes(),
                "destructiveHint": self.method == Method::DELETE,
                "openWorldHint": false,
            },
        })
    }

    /// The request this call stands for: method, path with query, body.
    ///
    /// # Errors
    ///
    /// A message for the caller when an argument is missing or malformed.
    pub fn request(
        &self,
        args: &Map<String, Value>,
    ) -> Result<(Method, String, Option<Value>), String> {
        let mut path = self.path.clone();
        for name in &self.path_params {
            let value = match args.get(name) {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => return Err(format!("`{name}` is required")),
            };
            path = path.replace(&format!("{{{name}}}"), &encode(&value));
        }
        let mut query = Vec::new();
        for name in &self.query_params {
            let values = match args.get(name) {
                None | Some(Value::Null) => continue,
                Some(Value::Array(items)) => items.iter().map(scalar).collect(),
                Some(v) => vec![scalar(v)],
            };
            for v in values {
                query.push(format!("{}={}", encode(name), encode(&v)));
            }
        }
        if !query.is_empty() {
            path = format!("{path}?{}", query.join("&"));
        }
        let body = match args.get("body") {
            Some(Value::Null) | None if self.has_body => Some(json!({})),
            Some(b) if self.has_body => Some(b.clone()),
            _ => None,
        };
        Ok((self.method.clone(), path, body))
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Percent-encodes everything but unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Paths no assistant gets a tool for: signing in, first-time setup, the
/// buyer's storefront, minting API keys, creating shops, and the payment
/// providers' webhooks.
const LEFT_OUT: &[&str] = &[
    "/api/auth/",
    "/api/setup",
    "/api/storefront/",
    "/api/api-keys",
    "/api/tenants",
    "/api/assistant/",
];

fn offered(path: &str) -> bool {
    path.starts_with("/api/")
        && !path.ends_with("/webhook")
        && !LEFT_OUT.iter().any(|p| path.starts_with(p))
}

/// One tool per operation in `spec` that takes JSON or nothing.
pub fn from_spec(spec: &Value) -> Vec<Tool> {
    let schemas = &spec["components"]["schemas"];
    let mut tools = Vec::new();
    let Some(paths) = spec["paths"].as_object() else {
        return tools;
    };
    for (path, item) in paths {
        if !offered(path) {
            continue;
        }
        let Some(ops) = item.as_object() else {
            continue;
        };
        for (verb, op) in ops {
            let Ok(method) = verb.to_uppercase().parse::<Method>() else {
                continue;
            };
            if let Some(tool) = tool(path, method, op, schemas) {
                tools.push(tool);
            }
        }
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

fn tool(path: &str, method: Method, op: &Value, schemas: &Value) -> Option<Tool> {
    let name = op["operationId"].as_str()?.to_owned();
    let body = &op["requestBody"];
    let body_schema = if body.is_null() {
        None
    } else {
        // Multipart uploads and other non-JSON bodies are not offered.
        Some(body["content"].get("application/json")?["schema"].clone())
    };
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut path_params = Vec::new();
    let mut query_params = Vec::new();
    for p in op["parameters"].as_array().into_iter().flatten() {
        let Some(pname) = p["name"].as_str() else {
            continue;
        };
        let mut schema = p["schema"].clone();
        if schema.is_null() {
            schema = json!({ "type": "string" });
        }
        if let Some(d) = p["description"].as_str() {
            schema["description"] = json!(d);
        }
        match p["in"].as_str() {
            Some("path") => {
                path_params.push(pname.to_owned());
                required.push(json!(pname));
            }
            Some("query") => {
                query_params.push(pname.to_owned());
                if p["required"] == true {
                    required.push(json!(pname));
                }
            }
            _ => continue,
        }
        properties.insert(pname.to_owned(), schema);
    }
    if let Some(schema) = &body_schema {
        properties.insert("body".to_owned(), schema.clone());
        if body["required"] == true {
            required.push(json!("body"));
        }
    }
    let mut input = json!({ "type": "object", "properties": properties, "required": required });
    let mut defs = Map::new();
    gather(&input, schemas, &mut defs);
    rewrite_refs(&mut input);
    if !defs.is_empty() {
        let mut defs = Value::Object(defs);
        rewrite_refs(&mut defs);
        input["$defs"] = defs;
    }
    let mut description = op["summary"].as_str().unwrap_or_default().to_owned();
    if let Some(more) = op["description"].as_str() {
        if !more.is_empty() && more != description {
            description = format!("{description}\n\n{more}");
        }
    }
    description = format!("{description}\n\n{method} {path}")
        .trim()
        .to_owned();
    Some(Tool {
        name,
        description,
        method,
        path: path.to_owned(),
        path_params,
        query_params,
        has_body: body_schema.is_some(),
        input_schema: input,
    })
}

const COMPONENTS: &str = "#/components/schemas/";

/// Copies every component schema `v` refers to, directly or not, into `defs`.
fn gather(v: &Value, schemas: &Value, defs: &mut Map<String, Value>) {
    match v {
        Value::Object(map) => {
            if let Some(name) = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix(COMPONENTS))
            {
                if !defs.contains_key(name) {
                    let schema = schemas[name].clone();
                    defs.insert(name.to_owned(), schema.clone());
                    gather(&schema, schemas, defs);
                }
            }
            for value in map.values() {
                gather(value, schemas, defs);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| gather(i, schemas, defs)),
        _ => {}
    }
}

fn rewrite_refs(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get_mut("$ref") {
                if let Some(name) = r.strip_prefix(COMPONENTS) {
                    *r = format!("#/$defs/{name}");
                }
            }
            map.values_mut().for_each(rewrite_refs);
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Value {
        json!({
            "paths": {
                "/api/products/{id}": {
                    "patch": {
                        "operationId": "update_product",
                        "summary": "Change a product.",
                        "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
                        "requestBody": { "required": true, "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/ProductPatch" } } } }
                    },
                    "get": { "operationId": "get_product", "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }] }
                },
                "/api/products": { "get": { "operationId": "list_products", "parameters": [
                    { "name": "tag", "in": "query", "schema": { "type": "string" } },
                    { "name": "limit", "in": "query", "schema": { "type": "integer" } }] } },
                "/api/files": { "post": { "operationId": "upload", "requestBody": {
                    "content": { "multipart/form-data": { "schema": {} } } } } },
                "/api/auth/login": { "post": { "operationId": "login" } },
                "/api/storefront/carts": { "post": { "operationId": "create_cart" } },
                "/api/payments/mollie/webhook": { "post": { "operationId": "receive" } }
            },
            "components": { "schemas": {
                "ProductPatch": { "type": "object", "properties": { "price": { "$ref": "#/components/schemas/Money" } } },
                "Money": { "type": "integer" },
                "Unused": { "type": "string" }
            } }
        })
    }

    #[test]
    fn one_tool_per_json_operation() {
        let tools = from_spec(&spec());
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["get_product", "list_products", "update_product"]);
        let update = &tools[2];
        assert!(update.writes());
        assert_eq!(update.input_schema["required"], json!(["id", "body"]));
        assert_eq!(
            update.input_schema["properties"]["body"]["$ref"],
            "#/$defs/ProductPatch"
        );
        let defs = update.input_schema["$defs"].as_object().unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(
            defs["ProductPatch"]["properties"]["price"]["$ref"],
            "#/$defs/Money"
        );
        assert!(update.description.ends_with("PATCH /api/products/{id}"));
        assert_eq!(tools[1].listing()["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn calls_become_requests() {
        let tools = from_spec(&spec());
        let args = |v: Value| v.as_object().unwrap().clone();
        let (m, path, body) = tools[2]
            .request(&args(
                json!({ "id": "prod_1/../x", "body": { "price": 5 } }),
            ))
            .unwrap();
        assert_eq!(m, Method::PATCH);
        assert_eq!(path, "/api/products/prod_1%2F..%2Fx");
        assert_eq!(body, Some(json!({ "price": 5 })));
        assert_eq!(
            tools[2].request(&args(json!({}))).unwrap_err(),
            "`id` is required"
        );
        let (_, path, body) = tools[1]
            .request(&args(json!({ "tag": "a b&c", "limit": 5, "other": 1 })))
            .unwrap();
        assert_eq!(path, "/api/products?tag=a%20b%26c&limit=5");
        assert_eq!(body, None);
    }
}
