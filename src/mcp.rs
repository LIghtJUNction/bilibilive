//! Bounded MCP stdio and HTTP sessions built on Project Z's zmcpio.
use crate::{app::App, schema};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use zmcpio::{
    json::{Document, Limits, Token},
    mcp::{Action, Context, Server},
    tools::{Content, Tool, ToolResult},
    transport::{FrameStatus, LineDecoder},
};

pub struct Session {
    app: App,
    server: Server<'static>,
}

const MAX_RESULT_BYTES: usize = 2 * 1024 * 1024;
const OUTPUT_BYTES: usize = 8 * 1024 * 1024;

impl Session {
    pub fn new(app: App) -> Self {
        Self {
            app,
            server: Server::new("bilibilive", crate::VERSION),
        }
    }

    pub fn handle(&mut self, input: &[u8]) -> Result<Option<Vec<u8>>, String> {
        if input.len() > 1024 * 1024 {
            return Err("MCP message exceeds 1 MiB".into());
        }
        let mut tokens = vec![Token::EMPTY; 16384];
        let mut output = vec![0u8; OUTPUT_BYTES];
        let action = match self.server.handle(input,&mut tokens,Limits::default(),&mut output) {
            Ok(action) => action,
            Err(error) => return Ok(Some(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("Invalid MCP request: {error}")}}).to_string().into_bytes())),
        };
        let length = match action {
            Action::Reply(length) => Some(length),
            Action::ListTools { context, cursor } => {
                if cursor.is_some() {
                    Some(
                        self.server
                            .error(context, -32602, "Invalid tool cursor", &mut output)
                            .map_err(|e| e.to_string())?,
                    )
                } else {
                    let specs: Vec<_> = schema::operations()
                        .into_iter()
                        .filter(|s| !self.app.is_read_only() || s.read_only)
                        .collect();
                    let strings: Vec<_> = specs.iter().map(|s| s.input.to_string()).collect();
                    let mut stores = vec![vec![Token::EMPTY; 1024]; strings.len()];
                    let documents: Vec<_> = strings
                        .iter()
                        .zip(stores.iter_mut())
                        .map(|(s, t)| Document::parse(s.as_bytes(), t, Limits::default()))
                        .collect::<Result<_, _>>()
                        .map_err(|e| e.to_string())?;
                    let tools: Vec<_> = specs
                        .iter()
                        .zip(documents.iter())
                        .map(|(s, d)| Tool::new(s.name, s.description, d.root()))
                        .collect::<Result<_, _>>()
                        .map_err(|e| e.to_string())?;
                    let size = self
                        .server
                        .list_tools(context, &tools, None, &mut output)
                        .map_err(|e| e.to_string())?;
                    // zmcpio preserves protocol envelopes. Application-specific
                    // read-only hints describe this dispatcher's actual policy.
                    let mut body: Value =
                        serde_json::from_slice(&output[..size]).map_err(|e| e.to_string())?;
                    if let Some(tools) = body
                        .pointer_mut("/result/tools")
                        .and_then(Value::as_array_mut)
                    {
                        for tool in tools {
                            if let Some(spec) = tool["name"].as_str().and_then(schema::find) {
                                tool["annotations"] = json!({"readOnlyHint":spec.read_only,"destructiveHint":!spec.read_only,"openWorldHint":true});
                            }
                        }
                    }
                    return Ok(Some(body.to_string().into_bytes()));
                }
            }
            Action::CallTool(call) => {
                let operation = schema::operations()
                    .into_iter()
                    .find(|s| call.name.matches(s.name));
                if let Some(operation) = operation {
                    let arguments: Value = serde_json::from_slice(call.arguments_bytes())
                        .map_err(|e| e.to_string())?;
                    let (value, failed) = match self.app.call(operation.name, arguments) {
                        Ok(value) => {
                            let failed = !crate::app::is_success(&value);
                            (value, failed)
                        }
                        Err(error) => (json!({"error":error}), true),
                    };
                    Some(self.encode_result(call.context, value, failed, &mut output)?)
                } else {
                    Some(
                        self.server
                            .error(
                                call.context,
                                -32602,
                                "Unknown bilibilive operation",
                                &mut output,
                            )
                            .map_err(|e| e.to_string())?,
                    )
                }
            }
            Action::Extension { context, .. } => Some(
                self.server
                    .error(context, -32601, "Method not found", &mut output)
                    .map_err(|e| e.to_string())?,
            ),
            Action::Notification(_) | Action::IncomingResponse(_) => None,
        };
        Ok(length.map(|length| output[..length].to_vec()))
    }

    fn encode_result(
        &self,
        context: Context<'_>,
        mut value: Value,
        failed: bool,
        output: &mut [u8],
    ) -> Result<usize, String> {
        if !value.is_object() {
            value = json!({"result":value});
        }
        let text = value.to_string();
        let mut result_tokens = vec![Token::EMPTY; 65536];
        let limits = Limits {
            max_bytes: MAX_RESULT_BYTES,
            max_depth: 48,
            max_members: 16384,
            max_string_bytes: MAX_RESULT_BYTES,
        };
        let encoded =
            Document::parse(text.as_bytes(), &mut result_tokens, limits).and_then(|document| {
                self.server.tool_result(
                    context,
                    ToolResult {
                        content: &[Content::Text(&text)],
                        structured_content: Some(document.root()),
                        is_error: failed,
                    },
                    output,
                )
            });
        match encoded {
            Ok(length) => Ok(length),
            Err(error) => {
                // A large screenshot or platform list must fail just this tool
                // invocation, leaving its MCP session available for later calls.
                let message = format!(
                    "Tool result exceeds MCP output limits ({error}); request a smaller result or use the local CLI output."
                );
                self.server
                    .tool_result(
                        context,
                        ToolResult {
                            content: &[Content::Text(&message)],
                            structured_content: None,
                            is_error: true,
                        },
                        output,
                    )
                    .map_err(|e| e.to_string())
            }
        }
    }
}

pub fn stdio(app: App) -> Result<(), String> {
    let mut session = Session::new(app);
    let mut storage = vec![0u8; 1024 * 1024];
    let mut decoder = LineDecoder::new(&mut storage, 1024 * 1024);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    loop {
        let bytes = input.fill_buf().map_err(|e| e.to_string())?;
        if bytes.is_empty() {
            decoder
                .finish()
                .map_err(|e| format!("Incomplete MCP input: {e:?}"))?;
            break;
        }
        let step = decoder.feed(bytes);
        input.consume(step.consumed);
        match step.status {
            FrameStatus::NeedInput => continue,
            FrameStatus::Ready => (),
            other => return Err(format!("MCP frame rejected: {other:?}")),
        }
        if let Some(reply) = session.handle(decoder.message().ok_or("Missing MCP frame")?)? {
            output.write_all(&reply).map_err(|e| e.to_string())?;
            output.write_all(b"\n").map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
        }
        decoder.consume();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(session: &mut Session, value: Value) -> Value {
        serde_json::from_slice(
            &session
                .handle(value.to_string().as_bytes())
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn initialized_stdio_session_lists_and_executes_the_shared_catalogue() {
        let app = App::new(Some(
            std::env::temp_dir().join(format!("bilibilive-mcp-test-{}", std::process::id())),
        ))
        .unwrap()
        .with_read_only(true);
        let mut session = Session::new(app);
        let init = request(
            &mut session,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "bilibilive");
        assert!(
            session
                .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap()
                .is_none()
        );
        let tools = request(
            &mut session,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        let tools = tools["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "config.get"));
        assert!(!tools.iter().any(|t| t["name"] == "live.start"));
        let response = request(
            &mut session,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"config.get","arguments":{}}}),
        );
        assert_eq!(response["result"]["structuredContent"]["protocol"], "rtmp");
        let denied = request(
            &mut session,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"live.start","arguments":{}}}),
        );
        assert_eq!(denied["result"]["isError"], true);
    }

    #[test]
    fn large_results_are_bounded_without_losing_the_session() {
        let app = App::new(Some(
            std::env::temp_dir().join(format!("bilibilive-mcp-output-test-{}", std::process::id())),
        ))
        .unwrap()
        .with_read_only(true);
        let mut session = Session::new(app);
        request(
            &mut session,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        );
        session
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .unwrap();
        let input=br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"obs.request","arguments":{"request_type":"GetSourceScreenshot"}}}"#;
        let mut tokens = vec![Token::EMPTY; 1024];
        let mut output = vec![0u8; OUTPUT_BYTES];
        let Action::CallTool(call) = session
            .server
            .handle(input, &mut tokens, Limits::default(), &mut output)
            .unwrap()
        else {
            panic!("expected tool call")
        };
        let length = session
            .encode_result(
                call.context,
                json!({"imageData":"x".repeat(100_000),"list":vec![1;1000]}),
                false,
                &mut output,
            )
            .unwrap();
        let accepted: Value = serde_json::from_slice(&output[..length]).unwrap();
        assert_ne!(accepted["result"]["isError"], true);
        assert_eq!(
            accepted["result"]["structuredContent"]["imageData"]
                .as_str()
                .unwrap()
                .len(),
            100_000
        );
        let length = session
            .encode_result(
                call.context,
                json!({"imageData":"x".repeat(MAX_RESULT_BYTES)}),
                false,
                &mut output,
            )
            .unwrap();
        let rejected: Value = serde_json::from_slice(&output[..length]).unwrap();
        assert_eq!(rejected["result"]["isError"], true);
        assert_eq!(rejected["id"], 2);
        let following = request(
            &mut session,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"config.get","arguments":{}}}),
        );
        assert_eq!(following["result"]["structuredContent"]["protocol"], "rtmp");
    }
}
