// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `SendStreamingMessage` and `SubscribeToTask`: a task's events over SSE.

use std::sync::Arc;

use rama::http::Response;
use serde_json::{Value, json};
use session_core::chat_json::{SseTx, json_stream_response};
use session_core::i18n::Lang;

use super::Call;
use super::envelope::{RpcError, rpc_body};
use super::tasks::{TaskView, task_view};
use crate::pages::turn_wait::{TurnWait, Waited};
use aiplane_runtime::agents::a2a::TaskState;
use aiplane_runtime::rama_server::state::RamaState;
use tokio::time::Instant;

fn sse_frame(id: &Value, result: Value) -> rama::bytes::Bytes {
    rama::bytes::Bytes::from(format!("data: {}\n\n", rpc_body(id, ("result", result))))
}

/// The task, then — once it is no longer working — its whole answer as one
/// artifact update and its final status; or its status alone when it failed,
/// was cancelled or waits for input. The answer is buffered behind the
/// output filter, so there is nothing to stream token by token.
pub(super) async fn stream(
    call: &Call,
    session_id: &str,
    task_id: &str,
) -> Result<Response, RpcError> {
    let first = task_view(
        &call.state,
        &call.served.agent.principal.id,
        call.lang,
        session_id,
        task_id,
        None,
    )
    .await?;
    let (tx, rx) = rama::futures::channel::mpsc::unbounded();
    let _ = tx.unbounded_send(Ok(sse_frame(&call.id, json!({ "task": first.task }))));
    let tail = Tail {
        state: call.state.clone(),
        lang: call.lang,
        id: call.id.clone(),
        principal_id: call.served.agent.principal.id.clone(),
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
    };
    if first.state == TaskState::Working {
        tokio::spawn(async move { tail.run(tx).await });
    } else {
        tail.finish(&first, &tx);
    }
    Ok(json_stream_response(rx))
}

struct Tail {
    state: Arc<RamaState>,
    lang: Lang,
    id: Value,
    principal_id: String,
    session_id: String,
    task_id: String,
}

impl Tail {
    async fn view(&self) -> Result<TaskView, RpcError> {
        task_view(
            &self.state,
            &self.principal_id,
            self.lang,
            &self.session_id,
            &self.task_id,
            None,
        )
        .await
    }

    async fn run(self, tx: SseTx) {
        let started = Instant::now();
        let wait = TurnWait {
            workers: &self.state.chats,
            principal_id: &self.principal_id,
            session_id: &self.session_id,
            turn_id: &self.task_id,
        };
        loop {
            match wait.next(&tx, started).await {
                Waited::Released => {}
                Waited::Expired | Waited::Gone => return,
            }
            match self.view().await {
                Ok(v) if v.state != TaskState::Working => {
                    self.finish(&v, &tx);
                    return;
                }
                // Claimed again since: its pause was answered.
                Ok(_) if wait.held() => {}
                // Working with nothing producing it: a turn orphaned by a
                // crash, which the next boot settles. Nothing will come.
                Ok(_) => return,
                Err(err) => {
                    tracing::warn!(error = ?err, task = %self.task_id, "a2a stream: reading the task");
                    return;
                }
            }
        }
    }

    fn finish(&self, v: &TaskView, tx: &SseTx) {
        if let Some(artifact) = v.task.get("artifacts").and_then(|a| a.get(0)) {
            let update = json!({ "artifactUpdate": {
                "taskId": self.task_id,
                "contextId": self.session_id,
                "artifact": artifact,
                "lastChunk": true,
            } });
            let _ = tx.unbounded_send(Ok(sse_frame(&self.id, update)));
        }
        let mut status = json!({ "statusUpdate": {
            "taskId": self.task_id,
            "contextId": self.session_id,
            "status": v.task["status"],
        } });
        if let Some(m) = v.task.get("metadata") {
            status["statusUpdate"]["metadata"] = m.clone();
        }
        let _ = tx.unbounded_send(Ok(sse_frame(&self.id, status)));
    }
}
