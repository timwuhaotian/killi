use super::*;

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
                "arguments": {"type": "object"}
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
                        self.native.insert(native.clone(), (name.clone(), tool_name.to_string()));
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
            return Output::error(format!("No MCP server `{server}`. Servers: {}.", self.names().join(", ")));
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
                            tool.get("inputSchema").map(Value::to_string).unwrap_or_else(|| "{}".into()),
                        ));
                    }
                    Output::ok(text).brief(format!("{} tools", tools.len()))
                }
                Err(e) => Output::error(format!("Could not reach MCP server {server}: {e:#}")),
            },
            Some(tool) => match Self::tool_arguments(args) {
                Ok(arguments) => self.call(server, tool, arguments, images).await,
                Err(e) => Output::error(e),
            },
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
        self.listed.borrow_mut().insert(server.to_string(), tools.clone());
        Ok(tools)
    }

    async fn call(&self, server: &str, tool: &str, arguments: Value, images: bool) -> Output {
        let connection = match self.connection(server).await {
            Ok(connection) => connection,
            Err(e) => return Output::error(format!("Could not reach MCP server {server}: {e:#}")),
        };
        let result = match connection.request("tools/call", json!({"name": tool, "arguments": arguments})).await {
            Ok(result) => result,
            Err(e) => return Output::error(format!("{server}/{tool} failed: {e:#}")),
        };
        let empty = Vec::new();
        let content = result["content"].as_array().unwrap_or(&empty);
        let (mut text, image) = Self::render_parts(content, images);
        if text.is_empty()
            && let Some(structured) = result.get("structuredContent")
        {
            text.push(structured.to_string());
        }
        let body = if text.is_empty() { "(no content)".to_string() } else { text.join("\n") };
        let mut output = if result["isError"] == true { Output::error(body) } else { Output::ok(body).brief(format!("{server}/{tool}")) };
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
                    image = Some(format!("data:{};base64,{}", part["mimeType"].as_str().unwrap_or("image/png"), part["data"].as_str().unwrap_or_default()));
                    text.push("(image attached below)".into());
                }
                Some("image") => text.push(image_note(part)),
                Some("resource") => text.push(part["resource"]["text"].as_str().map_or_else(|| part["resource"].to_string(), String::from)),
                _ => text.push(part.to_string()),
            }
        }
        (text, image)
    }

    /// The `arguments` for an MCP tool call: an object as sent, or a string holding one.
    /// Anything else (missing, or a value no tool takes) is no arguments, as before.
    pub(super) fn tool_arguments(args: &Value) -> Result<Value, String> {
        match args.get("arguments") {
            None | Some(Value::Null) => Ok(json!({})),
            Some(Value::Object(_)) => Ok(args["arguments"].clone()),
            Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                Ok(parsed @ Value::Object(_)) => Ok(parsed),
                Ok(_) => Err("mcp `arguments` is a string but not a JSON object; send an object.".into()),
                Err(e) => Err(format!("mcp `arguments` is a string but not JSON: {e}")),
            },
            Some(_) => Ok(json!({})),
        }
    }
}

/// An image the model isn't shown, named by type and size instead of dumped as base64.
fn image_note(part: &Value) -> String {
    let mime = part["mimeType"].as_str().unwrap_or("image/png");
    let kb = part["data"].as_str().map_or(0, |data| data.len() * 3 / 4 / 1024);
    format!("[image omitted: {mime} ({kb} KB not shown)]")
}

/// `mcp__server__tool`, limited to the characters and length tool names allow.
fn native_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(64)
        .collect()
}
