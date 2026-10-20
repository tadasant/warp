//! A local stand-in for a domain that adopted SEP-2127: serves an AI Catalog, a Server Card and a
//! minimal streamable-HTTP MCP server (JSON responses only) from one plain-HTTP origin.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

pub const FIXTURE_TOOL_NAME: &str = "search_fixture_docs";

pub struct FixtureCatalogServer {
    origin: String,
    tool_calls: Arc<AtomicUsize>,
    authorization_headers: Arc<Mutex<Vec<String>>>,
}

impl FixtureCatalogServer {
    /// Binds `127.0.0.1:<port>` (0 for ephemeral) and serves on a background thread. When
    /// `required_bearer` is set, the MCP endpoint answers 401 unless that bearer token is sent.
    pub fn start(port: u16, required_bearer: Option<String>) -> std::io::Result<Self> {
        Self::start_with_card_auth(port, required_bearer, true)
    }

    /// Like [`Self::start`], but `declare_auth = false` leaves the auth requirement out of the
    /// Server Card even though the endpoint still demands a bearer token, as GitHub's card does.
    pub fn start_with_card_auth(
        port: u16,
        required_bearer: Option<String>,
        declare_auth: bool,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let origin = format!("http://{}", listener.local_addr()?);
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let authorization_headers = Arc::new(Mutex::new(Vec::new()));
        let state = ServerState {
            origin: origin.clone(),
            required_bearer,
            declare_auth,
            tool_calls: tool_calls.clone(),
            authorization_headers: authorization_headers.clone(),
        };
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = state.clone();
                thread::spawn(move || {
                    let _ = state.handle(stream);
                });
            }
        });
        Ok(Self {
            origin,
            tool_calls,
            authorization_headers,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    pub fn tool_calls(&self) -> usize {
        self.tool_calls.load(Ordering::SeqCst)
    }

    /// `Authorization` header values received by the MCP endpoint.
    pub fn authorization_headers(&self) -> Vec<String> {
        self.authorization_headers.lock().unwrap().clone()
    }
}

#[derive(Clone)]
struct ServerState {
    origin: String,
    required_bearer: Option<String>,
    declare_auth: bool,
    tool_calls: Arc<AtomicUsize>,
    authorization_headers: Arc<Mutex<Vec<String>>>,
}

impl ServerState {
    fn handle(&self, mut stream: TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        loop {
            let mut request_line = String::new();
            if reader.read_line(&mut request_line)? == 0 {
                return Ok(());
            }
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or("/").to_string();
            let mut content_length = 0;
            let mut authorization = None;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header)? == 0 || header == "\r\n" {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    let value = value.trim().to_string();
                    match name.trim().to_ascii_lowercase().as_str() {
                        "content-length" => content_length = value.parse().unwrap_or(0),
                        "authorization" => authorization = Some(value),
                        _ => {}
                    }
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body)?;

            let (status, response_body) = self.route(&method, &path, authorization, &body);
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{response_body}",
                response_body.len()
            );
            stream.write_all(response.as_bytes())?;
            stream.flush()?;
        }
    }

    fn route(
        &self,
        method: &str,
        path: &str,
        authorization: Option<String>,
        body: &[u8],
    ) -> (&'static str, String) {
        match (method, path) {
            ("GET", "/.well-known/ai-catalog.json") => ("200 OK", self.catalog().to_string()),
            ("GET", "/mcp/server-card") => ("200 OK", self.server_card().to_string()),
            ("POST", "/mcp") => {
                if let Some(authorization) = &authorization {
                    self.authorization_headers
                        .lock()
                        .unwrap()
                        .push(authorization.clone());
                }
                if let Some(token) = &self.required_bearer
                    && authorization.as_deref() != Some(format!("Bearer {token}").as_str())
                {
                    return ("401 Unauthorized", String::new());
                }
                let Ok(request) = serde_json::from_slice::<Value>(body) else {
                    return ("400 Bad Request", String::new());
                };
                match self.mcp_response(&request) {
                    Some(response) => ("200 OK", response.to_string()),
                    None => ("202 Accepted", String::new()),
                }
            }
            ("GET", "/mcp") => ("405 Method Not Allowed", String::new()),
            _ => ("404 Not Found", String::new()),
        }
    }

    fn catalog(&self) -> Value {
        json!({
            "version": "1",
            "servers": [ { "serverCard": "/mcp/server-card" } ],
            "skills": [ {
                "name": "Fixture Skills",
                "description": "Skills advertised by the local fixture domain.",
                "skills": [ { "name": "fixture-skill", "description": "Does fixture things." } ]
            } ]
        })
    }

    fn server_card(&self) -> Value {
        let mut remote = json!({
            "type": "streamable-http",
            "url": format!("{}/mcp", self.origin),
        });
        if self.required_bearer.is_some() && self.declare_auth {
            remote["auth"] = json!({
                "kind": "header",
                "header": "Authorization",
                "prefix": "Bearer ",
                "variable": "fixture_token",
                "instructions": "Paste the fixture token."
            });
        }
        json!({
            "name": "dev.warp.fixture/docs",
            "version": "0.1.0",
            "title": "Fixture Docs",
            "description": "Local fixture MCP server used to exercise AI Catalog discovery.",
            "remote": remote
        })
    }

    /// `None` for notifications, which get a 202 with no body.
    fn mcp_response(&self, request: &Value) -> Option<Value> {
        let id = request.get("id")?.clone();
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": request["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or("2025-03-26"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "ai-catalog-fixture", "version": "0.1.0" }
            }),
            "tools/list" => json!({
                "tools": [ {
                    "name": FIXTURE_TOOL_NAME,
                    "description": "Searches the fixture docs.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "query": { "type": "string" } },
                        "required": ["query"]
                    }
                } ]
            }),
            "tools/call" => {
                self.tool_calls.fetch_add(1, Ordering::SeqCst);
                let query = request["params"]["arguments"]["query"]
                    .as_str()
                    .unwrap_or_default();
                json!({
                    "content": [ { "type": "text", "text": format!("Fixture result for '{query}'") } ],
                    "isError": false
                })
            }
            "resources/list" => json!({ "resources": [] }),
            "resources/templates/list" => json!({ "resourceTemplates": [] }),
            "prompts/list" => json!({ "prompts": [] }),
            "ping" => json!({}),
            method => {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("unknown method {method}") }
                }));
            }
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
}
