//! This crate's MCP tools reached in the same process, with no socket: the
//! voice agent's functions (`voice_tools`) and the phone's Midas device link
//! (the client's `systems::midas_link`) both talk to them this way.
//!
//! **Not a second tool surface.** Each caller opens an MCP session on an
//! in-memory pipe to a fresh [`Tools`], exactly as an editor's session on the
//! socket gets one, and lists and calls tools through it. So the names, the
//! argument schemas and the wording of a reply are the same everywhere, and a
//! call goes down the same job channel to the same checks in the client.

use std::sync::Arc;

use futures_channel::mpsc::UnboundedSender;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};

use crate::protocol::Job;
use crate::server::Tools;

/// The pipe's buffer: a picture is the largest message, and a reader on the
/// other end drains it as it comes.
const PIPE_BYTES: usize = 1 << 20;

/// An open session with the tools.
pub type Session = Arc<RunningService<RoleClient, ()>>;

/// Open a session on the calling tokio runtime, which is where its tasks run.
pub async fn open(jobs: UnboundedSender<Job>) -> Result<Session, String> {
    let (ours, theirs) = tokio::io::duplex(PIPE_BYTES);
    let tools = Tools::new(jobs);
    tokio::spawn(async move {
        match tools.serve(theirs).await {
            Ok(s) => {
                let _ = s.waiting().await;
            }
            Err(e) => tracing::warn!(error = %e, "mcp: no in-process session"),
        }
    });
    ().serve(ours)
        .await
        .map(Arc::new)
        .map_err(|e| format!("cannot open the tools: {e}"))
}

/// The tools for a relay that speaks MCP's JSON itself: listed and answered as
/// the protocol words them, so a relay passes them on untouched.
pub struct LocalTools {
    jobs: UnboundedSender<Job>,
    session: tokio::sync::OnceCell<Result<Session, String>>,
}

impl LocalTools {
    #[must_use]
    pub fn new(jobs: UnboundedSender<Job>) -> Self {
        Self {
            jobs,
            session: tokio::sync::OnceCell::new(),
        }
    }

    async fn session(&self) -> Result<Session, String> {
        self.session
            .get_or_init(|| open(self.jobs.clone()))
            .await
            .clone()
    }

    /// Every tool, each as `tools/list` gives it: name, description, `inputSchema`.
    pub async fn list(&self) -> Result<Vec<serde_json::Value>, String> {
        let tools = self
            .session()
            .await?
            .list_all_tools()
            .await
            .map_err(|e| format!("cannot list the tools: {e}"))?;
        tools
            .into_iter()
            .map(|t| serde_json::to_value(t).map_err(|e| e.to_string()))
            .collect()
    }

    /// Call one: the `result` of a `tools/call` response, content blocks and all.
    pub async fn call(
        &self,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let session = self.session().await?;
        let mut params = CallToolRequestParams::new(name);
        params.arguments = match arguments {
            serde_json::Value::Object(o) => Some(o),
            _ => None,
        };
        let result = session
            .call_tool(params)
            .await
            .map_err(|e| format!("the tool failed: {e}"))?;
        serde_json::to_value(result).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{AgentReply, AgentRequest};
    use crate::protocol::{Reply, Request};
    use futures_util::StreamExt as _;

    /// A relay sees the tools as MCP words them, and a call's answer is a
    /// `tools/call` result it can forward as it is.
    #[test]
    fn a_relay_lists_and_calls_the_tools_as_json() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<Job>();
        rt.block_on(async move {
            tokio::spawn(async move {
                while let Some(job) = rx.next().await {
                    let text = match job.request {
                        Request::Agent(AgentRequest::Me) => "You are jeroen.",
                        _ => "something else",
                    };
                    let _ = job.reply.send(Reply::Agent(Box::new(AgentReply::ok(
                        text,
                        serde_json::Value::Null,
                    ))));
                }
            });
            let tools = LocalTools::new(tx);
            let listed = tools.list().await.unwrap();
            for name in ["me", "walk_to", "view", "view_here", "state"] {
                assert!(listed.iter().any(|t| t["name"] == name), "{name} is listed");
            }
            let walk = listed.iter().find(|t| t["name"] == "walk_to").unwrap();
            assert_eq!(walk["inputSchema"]["type"], "object");
            let me = tools
                .call("me".into(), serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(me["content"][0]["type"], "text");
            assert_eq!(me["content"][0]["text"], "You are jeroen.");
            assert_ne!(me["isError"], true);
        });
    }
}
