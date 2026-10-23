//! The voice agent's tools: this crate's own MCP tools, reached through an MCP
//! client in the same process.
//!
//! **Not a second tool surface.** The voice agent could have been given its
//! own table of functions mapped onto `AgentRequest`s; that table would drift
//! from the MCP tools the text agent uses, in names, in argument schemas and in
//! how a reply is worded. Instead each voice call opens an MCP session on an
//! in-memory pipe to a fresh [`Tools`], exactly as an editor's session on the
//! socket gets one, and lists and calls tools through it. Whatever the text
//! agent can do, the voice agent does the same way, through the same job
//! channel and the same checks in the client.

use std::sync::Arc;

use cartopolis_voice::{BoxFuture, ToolHost, ToolOutput, ToolSpec};
use futures_channel::mpsc::UnboundedSender;
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};

use crate::protocol::Job;
use crate::server::Tools;

/// The pipe's buffer: a picture is the largest message, and a reader on the
/// other end drains it as it comes.
const PIPE_BYTES: usize = 1 << 20;

/// A [`ToolHost`] backed by this client's MCP tools.
pub struct McpTools {
    jobs: UnboundedSender<Job>,
    session: tokio::sync::OnceCell<Result<Arc<RunningService<RoleClient, ()>>, String>>,
}

impl McpTools {
    #[must_use]
    pub fn new(jobs: UnboundedSender<Job>) -> Self {
        Self {
            jobs,
            session: tokio::sync::OnceCell::new(),
        }
    }

    /// The session, opened on first use — on the voice agent's runtime, which
    /// is where its tasks must run.
    async fn session(&self) -> Result<Arc<RunningService<RoleClient, ()>>, String> {
        self.session
            .get_or_init(|| async {
                let (ours, theirs) = tokio::io::duplex(PIPE_BYTES);
                let tools = Tools::new(self.jobs.clone());
                tokio::spawn(async move {
                    match tools.serve(theirs).await {
                        Ok(s) => {
                            let _ = s.waiting().await;
                        }
                        Err(e) => tracing::warn!(error = %e, "voice tools: no session"),
                    }
                });
                ().serve(ours)
                    .await
                    .map(Arc::new)
                    .map_err(|e| format!("cannot open the tools: {e}"))
            })
            .await
            .clone()
    }
}

impl ToolHost for McpTools {
    fn list(&self) -> BoxFuture<'_, Result<Vec<ToolSpec>, String>> {
        Box::pin(async {
            let session = self.session().await?;
            let tools = session
                .list_all_tools()
                .await
                .map_err(|e| format!("cannot list the tools: {e}"))?;
            Ok(tools
                .into_iter()
                .map(|t| ToolSpec {
                    name: t.name.to_string(),
                    description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                    parameters: serde_json::Value::Object((*t.input_schema).clone()),
                })
                .collect())
        })
    }

    fn call(&self, name: String, arguments: serde_json::Value) -> BoxFuture<'_, ToolOutput> {
        Box::pin(async move {
            let session = match self.session().await {
                Ok(s) => s,
                Err(e) => return failed(e),
            };
            let mut params = CallToolRequestParams::new(name);
            params.arguments = match arguments {
                serde_json::Value::Object(o) => Some(o),
                _ => None,
            };
            match session.call_tool(params).await {
                Ok(result) => {
                    let mut out = ToolOutput {
                        error: result.is_error == Some(true),
                        ..ToolOutput::default()
                    };
                    for block in result.content {
                        match block {
                            ContentBlock::Text(t) => {
                                if !out.text.is_empty() {
                                    out.text.push('\n');
                                }
                                out.text.push_str(&t.text);
                            }
                            ContentBlock::Image(i) if i.mime_type == "image/jpeg" => {
                                out.image.get_or_insert(i.data);
                            }
                            _ => {}
                        }
                    }
                    out
                }
                Err(e) => failed(format!("the tool failed: {e}")),
            }
        })
    }
}

fn failed(text: String) -> ToolOutput {
    ToolOutput {
        text,
        image: None,
        error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{AgentReply, AgentRequest};
    use crate::protocol::{Reply, Request};
    use futures_util::StreamExt as _;

    /// The voice agent sees the same tools as an editor, and a call goes down
    /// the same job channel as theirs does.
    #[test]
    fn the_voice_agent_lists_and_calls_the_mcp_tools() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<Job>();
        rt.block_on(async move {
            // The client's side of the channel: answer one `me`.
            tokio::spawn(async move {
                while let Some(job) = rx.next().await {
                    let text = match job.request {
                        Request::Agent(AgentRequest::Me) => "You are viberfox.",
                        _ => "something else",
                    };
                    let _ = job.reply.send(Reply::Agent(Box::new(AgentReply::ok(
                        text,
                        serde_json::Value::Null,
                    ))));
                }
            });
            let host = McpTools::new(tx);
            let tools = host.list().await.unwrap();
            for name in ["me", "look_around", "walk_to", "voice_call", "hang_up"] {
                assert!(tools.iter().any(|t| t.name == name), "{name} is listed");
            }
            let walk = tools.iter().find(|t| t.name == "walk_to").unwrap();
            assert_eq!(walk.parameters["type"], "object");
            let out = host.call("me".into(), serde_json::json!({})).await;
            assert!(!out.error);
            assert_eq!(out.text, "You are viberfox.");
            let out = host.call("no_such_tool".into(), serde_json::json!({})).await;
            assert!(out.error);
        });
    }
}
