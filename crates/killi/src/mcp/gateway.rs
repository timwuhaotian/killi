use super::*;

use super::connection::RpcError;
use crate::tools::Output;

impl Mcp {
    fn refused(&self, server: &str) -> Option<Output> {
        self.off
            .borrow()
            .contains(server)
            .then(|| Output::error(format!("The user turned off the MCP server `{server}` with /mcp. Don't use it unless they turn it on again.")))
    }

    /// The `mcp` gateway tool, naming the servers it can reach.
    pub fn definition(&self) -> Option<Value> {
        if self.servers.is_empty() {
            return None;
        }
        let names = match self.names().join(", ") {
            names if names.is_empty() => "none are on now".to_string(),
            names => names,
        };
        Some(json!({"type": "function", "function": {
            "name": "mcp",
            "description": format!("Use tools on MCP servers: {names}. Call with only `server` to list its tools and their arguments, then with `tool` and `arguments` to run one."),
            "parameters": {"type": "object", "properties": {
                "server": {"type": "string"},
                "tool": {"type": "string"},
                "arguments": {"type": "object", "description": "The tool's arguments as a JSON object, exactly as its `Arguments:` schema shows."}
            }, "required": ["server"]}
        }}))
    }

    /// Starts the named servers now and returns native definitions of their tools. The
    /// servers start together, so one slow server doesn't hold the others behind it.
    pub async fn start_eager(&mut self, names: &[String]) -> (Vec<Value>, Vec<String>) {
        let mut definitions = Vec::new();
        let mut problems = Vec::new();
        let started = futures_util::future::join_all(names.iter().map(|name| {
            let tools = self.tools(name);
            async move { (name, tools.await) }
        }))
        .await;
        for (name, tools) in started {
            match tools {
                Ok(tools) => {
                    for tool in tools {
                        let tool_name = tool["name"].as_str().unwrap_or_default();
                        let native = native_name(name, tool_name);
                        self.native
                            .insert(native.clone(), (name.clone(), tool_name.to_string()));
                        definitions.push(json!({"type": "function", "function": {
                            "name": native,
                            "description": tool["description"].as_str().unwrap_or_default(),
                            "parameters": tool.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object"})),
                        }}));
                    }
                }
                Err(e) => problems.push(format!("MCP server {name}: {e:#}")),
            }
        }
        (definitions, problems)
    }

    pub fn is_native(&self, name: &str) -> bool {
        self.native.contains_key(name)
    }

    pub async fn call_native(&self, name: &str, arguments: Value, images: bool) -> Output {
        let Some((server, tool)) = self.native.get(name) else {
            return Output::error(format!("Unknown tool `{name}`."));
        };
        if let Some(refused) = self.refused(server) {
            return refused;
        }
        self.call(server, tool, arguments, images).await
    }

    /// The `mcp` tool.
    pub async fn run(&self, args: &Value, images: bool) -> Output {
        let Some(server) = args["server"].as_str() else {
            return Output::error("mcp needs `server`.");
        };
        if !self.servers.contains_key(server) {
            return Output::error(format!(
                "No MCP server `{server}`. Servers: {}.",
                self.names().join(", ")
            ));
        }
        if let Some(refused) = self.refused(server) {
            return refused;
        }
        match args["tool"].as_str().filter(|t| !t.is_empty()) {
            None => match self.tools(server).await {
                Ok(tools) => {
                    let mut text = format!("Tools on {server}:\n");
                    for tool in &tools {
                        text.push_str(&format!(
                            "\n## {}\n{}\nArguments: {}\n",
                            tool["name"].as_str().unwrap_or_default(),
                            tool["description"].as_str().unwrap_or_default().trim(),
                            tool.get("inputSchema")
                                .map(Value::to_string)
                                .unwrap_or_else(|| "{}".into()),
                        ));
                    }
                    Output::ok(text).brief(format!("{} tools", tools.len()))
                }
                Err(e) => Output::error(format!("Could not reach MCP server {server}: {e:#}")),
            },
            Some(tool) => match self.tool_arguments(server, tool, args).await {
                Ok(arguments) => self.call(server, tool, arguments, images).await,
                Err(refused) => refused,
            },
        }
    }

    /// One of `server`'s tools as it describes itself, when the server can be reached.
    async fn tool_schema(&self, server: &str, tool: &str) -> Option<Value> {
        self.tools(server)
            .await
            .ok()?
            .into_iter()
            .find(|t| t["name"].as_str() == Some(tool))
    }

    /// What `tool` takes, from its input schema, for a call that came without arguments.
    async fn shape(&self, server: &str, tool: &str) -> Shape {
        let schema = self.tool_schema(server, tool).await;
        Shape::of(schema.as_ref().and_then(|t| t.get("inputSchema")))
    }

    /// The `arguments` a tool call sends. A model that omits them, sends an empty string or
    /// malformed JSON is told what the tool takes instead of being handed a server error
    /// it can't read; a tool that takes nothing still runs with no arguments.
    async fn tool_arguments(&self, server: &str, tool: &str, args: &Value) -> Result<Value, Output> {
        let sent = match Self::sent_arguments(args) {
            Ok(sent) => sent,
            Err(problem) => {
                return Err(Output::error(format!(
                    "{problem}\n{}",
                    self.shape(server, tool).await.text
                )));
            }
        };
        if let Some(arguments) = sent {
            return Ok(arguments);
        }
        let shape = self.shape(server, tool).await;
        if shape.required.is_empty() {
            return Ok(json!({}));
        }
        Err(Output::error(format!(
            "mcp needs `arguments` for {tool}:\n{}",
            shape.text
        )))
    }

    /// What the model sent as `arguments`: the object to send, or nothing a tool takes when
    /// it is missing, empty or a value no tool takes, with the problem to report when it
    /// sent something unreadable.
    pub(super) fn sent_arguments(args: &Value) -> Result<Option<Value>, String> {
        match args.get("arguments") {
            Some(value @ Value::Object(_)) => Ok(Some(value.clone())),
            Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
            Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
                Ok(parsed @ Value::Object(_)) => Ok(Some(parsed)),
                Ok(_) => Err(
                    "mcp `arguments` is a string but not a JSON object; send an object.".into(),
                ),
                Err(e) => Err(format!("mcp `arguments` is a string but not JSON: {e}")),
            },
            _ => Ok(None),
        }
    }

    async fn tools(&self, server: &str) -> Result<Vec<Value>> {
        if let Some(tools) = self.listed.borrow().get(server) {
            return Ok(tools.clone());
        }
        let connection = self.connection(server).await?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = connection.request("tools/list", params).await?;
            tools.extend(result["tools"].as_array().cloned().unwrap_or_default());
            match result["nextCursor"].as_str() {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => break,
            }
        }
        self.listed
            .borrow_mut()
            .insert(server.to_string(), tools.clone());
        Ok(tools)
    }

    async fn call(&self, server: &str, tool: &str, arguments: Value, images: bool) -> Output {
        let connection = match self.connection(server).await {
            Ok(connection) => connection,
            Err(e) => return Output::error(format!("Could not reach MCP server {server}: {e:#}")),
        };
        let result = match connection
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
        {
            Ok(result) => result,
            // A server that rejects the arguments it was sent is told what the tool takes,
            // so the next call can carry them.
            Err(e) => {
                let mut text = format!("{server}/{tool} failed: {e:#}");
                if e.downcast_ref::<RpcError>()
                    .is_some_and(|error| error.code == Some(-32602))
                {
                    text.push_str(&format!("\n{}", self.shape(server, tool).await.text));
                }
                return Output::error(text);
            }
        };
        let empty = Vec::new();
        let content = result["content"].as_array().unwrap_or(&empty);
        let (mut text, image) = Self::render_parts(content, images);
        if text.is_empty()
            && let Some(structured) = result.get("structuredContent")
        {
            text.push(structured.to_string());
        }
        let body = if text.is_empty() {
            "(no content)".to_string()
        } else {
            text.join("\n")
        };
        let mut output = if result["isError"] == true {
            Output::error(body)
        } else {
            Output::ok(body).brief(format!("{server}/{tool}"))
        };
        output.image = image;
        output
    }

    /// The text lines of an MCP result and the image to attach, if any: the first image
    /// when the model sees images, every other one named and never dumped as base64.
    pub(super) fn render_parts(content: &[Value], images: bool) -> (Vec<String>, Option<String>) {
        let mut text = Vec::new();
        let mut image = None;
        for part in content {
            match part["type"].as_str() {
                Some("text") => text.push(part["text"].as_str().unwrap_or_default().to_string()),
                Some("image") if images && image.is_none() => {
                    image = Some(format!(
                        "data:{};base64,{}",
                        part["mimeType"].as_str().unwrap_or("image/png"),
                        part["data"].as_str().unwrap_or_default()
                    ));
                    text.push("(image attached below)".into());
                }
                Some("image") => text.push(image_note(part)),
                Some("resource") => text.push(
                    part["resource"]["text"]
                        .as_str()
                        .map_or_else(|| part["resource"].to_string(), String::from),
                ),
                _ => text.push(part.to_string()),
            }
        }
        (text, image)
    }
}

/// What a tool takes, as the model must send it: the properties with their types, which of
/// them are required, and an object to copy.
struct Shape {
    /// What the model reads.
    text: String,
    /// The properties the tool requires; empty when it takes none.
    required: Vec<String>,
}

impl Shape {
    /// The shape from a tool's input schema; `None` when the server couldn't say.
    fn of(schema: Option<&Value>) -> Self {
        let is_required = |property: &str| {
            schema
                .and_then(|s| s["required"].as_array())
                .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(property)))
        };
        let Some(properties) = schema.and_then(|s| s["properties"].as_object()) else {
            return Self {
                text: format!(
                    "its schema names no arguments ({}); send `arguments` as an object.",
                    crate::llm::truncate(&schema.map_or_else(|| "{}".into(), Value::to_string), 300)
                ),
                required: Vec::new(),
            };
        };
        let required: Vec<String> = properties
            .keys()
            .filter(|property| is_required(property))
            .cloned()
            .collect();
        let optional: Vec<String> = properties
            .keys()
            .filter(|property| !is_required(property))
            .cloned()
            .collect();
        // The required ones come first: they are what the call can't go without.
        let takes: Vec<String> = required
            .iter()
            .chain(optional.iter())
            .map(|property| {
                format!(
                    "{property} ({}{})",
                    kind(&properties[property]),
                    if required.contains(property) { ", required" } else { "" }
                )
            })
            .collect();
        // An object to copy: the required properties when there are any, else every one.
        let example: Vec<String> = if required.is_empty() {
            optional
        } else {
            required.clone()
        }
        .iter()
        .map(|property| format!("\"{property}\": \"<{}>\"", kind(&properties[property])))
        .collect();
        Self {
            text: format!(
                "{}. Send `arguments` as a JSON object, e.g. {{{}}}.",
                takes.join(", "),
                example.join(", ")
            ),
            required,
        }
    }
}

/// A property's type as its schema names it.
fn kind(property: &Value) -> &str {
    property["type"].as_str().unwrap_or("value")
}

/// An image the model isn't shown, named by type and size instead of dumped as base64.
fn image_note(part: &Value) -> String {
    let mime = part["mimeType"].as_str().unwrap_or("image/png");
    let kb = part["data"]
        .as_str()
        .map_or(0, |data| data.len() * 3 / 4 / 1024);
    format!("[image omitted: {mime} ({kb} KB not shown)]")
}

/// `mcp__server__tool`, limited to the characters and length tool names allow.
fn native_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}
