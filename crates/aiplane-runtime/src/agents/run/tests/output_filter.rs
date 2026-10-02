// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The output filter against identifiers the model put into the turn
//! itself: a tool that repeats its arguments, in an error or in a result,
//! and a sub-agent that repeats its task, establish nothing.

use shared::api::ToolDef;

use super::*;
use crate::server::tools::{Tool, ToolContext, ToolError, ToolFuture};

const LOOKUP: &str = "lookup_invoice";
const ECHO: &str = "company_echo";

/// An invoice lookup that knows one invoice and one customer's invoices,
/// formats a bare number as an invoice, names a void invoice's replacement
/// in its error, and names the one it was asked for when it knows none.
struct LookupInvoice;

impl Tool for LookupInvoice {
    fn id(&self) -> &str {
        LOOKUP
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(
            LOOKUP,
            "Look up an invoice by its number.",
            json!({ "type": "object",
                    "properties": { "id": {}, "customer": { "type": "string" } } }),
        )
    }

    fn run<'a>(&'a self, _ctx: ToolContext, args: Value) -> ToolFuture<'a> {
        Box::pin(async move {
            if args["customer"] == "K-12345" {
                return Ok(json!({ "customer": "K-12345", "invoices": ["RE-123456"] }));
            }
            if let Some(n) = args["id"].as_u64() {
                return Ok(json!({ "invoice": format!("RE-{n}"), "status": "paid" }));
            }
            match args["id"].as_str().unwrap_or_default() {
                "RE-500" => Ok(json!({ "invoice": "RE-500", "replaced_by": "RE-501" })),
                "RE-404" => Err(ToolError::Failed(
                    "invoice RE-404 is void; it was replaced by RE-405".into(),
                )),
                id => Err(ToolError::Failed(format!("no invoice {id} found"))),
            }
        })
    }
}

/// One visitor message to an agent with [`LookupInvoice`] and the echo tool,
/// whose model answers with `rounds`.
async fn lookup_agent_says(action: &str, rounds: Vec<Value>, message: &str) -> AgentReply {
    let main = llm(rounds).await;
    let tools = base_tools().with(LookupInvoice);
    let world = World::build(
        &[("support-pool", "support-model", &main)],
        None,
        false,
        tools,
        None,
    )
    .await;
    let agent = world
        .agent(
            "support",
            &[
                (GrantKind::Pool, "support-pool"),
                (GrantKind::Tool, LOOKUP),
                (GrantKind::Tool, ECHO),
            ],
        )
        .await;
    world
        .publish(
            &agent,
            &json!({
                "main": {
                    "pool": "support-pool",
                    "instructions": { "orchestration": "Look invoices up, then answer." },
                    "tools": [LOOKUP, ECHO],
                    "budget": { "rounds": 6 }
                },
                "publish": invoice_filter(Some(action)),
            }),
        )
        .await;
    run_turn(
        &world.state,
        AgentTurn {
            agent_id: &agent,
            session_id: None,
            message,
            visitor_id: None,
        },
    )
    .await
    .unwrap()
}

fn withheld() -> String {
    session_core::i18n::t(session_core::i18n::Lang::En, "agent-output-withheld")
}

fn redacted() -> String {
    session_core::i18n::t(session_core::i18n::Lang::En, "agent-output-redacted")
}

#[tokio::test]
async fn an_identifier_an_errored_call_repeats_from_its_arguments_is_withheld() {
    let reply = lookup_agent_says(
        "withhold",
        vec![
            call("l1", LOOKUP, json!({"id": "RE-99999"})),
            text("Your invoice RE-99999 is paid."),
        ],
        "My invoice is RE-99999, is it paid?",
    )
    .await;
    assert_eq!(reply.answer, Some(withheld()), "{reply:?}");
}

#[tokio::test]
async fn an_identifier_a_successful_call_merely_echoes_is_withheld() {
    let reply = lookup_agent_says(
        "withhold",
        vec![
            call("e1", ECHO, json!({"message": "RE-99999"})),
            text("Your invoice RE-99999 is paid."),
        ],
        "My invoice is RE-99999, is it paid?",
    )
    .await;
    assert_eq!(reply.answer, Some(withheld()), "{reply:?}");
}

#[tokio::test]
async fn a_lookup_vouches_for_what_it_found_not_for_what_it_was_asked() {
    let reply = lookup_agent_says(
        "redact",
        vec![
            call("l1", LOOKUP, json!({"id": "RE-500"})),
            text("RE-500 was replaced by RE-501."),
        ],
        "What happened to RE-500?",
    )
    .await;
    assert_eq!(
        reply.answer,
        Some(format!("{} was replaced by RE-501.", redacted())),
        "{reply:?}"
    );
}

#[tokio::test]
async fn a_sub_agents_looked_up_invoice_passes_and_its_echo_of_the_task_does_not() {
    let sub_rounds = || {
        vec![
            call(
                "b1",
                INVOICES,
                json!({"customer_id": "K-12345", "year": 2026}),
            ),
            finish(
                "b2",
                json!({"answer": "RE-1 was billed twice; RE-77 is not on file."}),
            ),
        ]
    };
    let looked_up = support_example_scripted(
        Some(invoice_filter(None)),
        "Charged twice on RE-77",
        sub_rounds(),
        "RE-1 was billed twice and is being refunded.",
    )
    .await;
    assert_eq!(
        looked_up.second.answer.as_deref(),
        Some("RE-1 was billed twice and is being refunded.")
    );

    let repeated = support_example_scripted(
        Some(invoice_filter(None)),
        "Charged twice on RE-77",
        sub_rounds(),
        "RE-77 was billed twice and is being refunded.",
    )
    .await;
    assert_eq!(repeated.second.answer, Some(withheld()));
    assert_eq!(blocked_events(&repeated).await.len(), 1);
}

#[tokio::test]
async fn a_sub_agent_finish_that_only_repeats_its_task_establishes_nothing() {
    let s = support_example_scripted(
        Some(invoice_filter(None)),
        "Charged twice on RE-77",
        vec![finish(
            "b1",
            json!({"answer": "Invoice RE-77 was billed twice; a refund is issued."}),
        )],
        "RE-77 is being refunded.",
    )
    .await;
    assert_eq!(s.second.answer, Some(withheld()));
}

#[tokio::test]
async fn an_identifier_only_a_failed_call_names_is_withheld() {
    let reply = lookup_agent_says(
        "withhold",
        vec![
            call("l1", LOOKUP, json!({"id": "RE-404"})),
            text("Your invoice was replaced by RE-405."),
        ],
        "What happened to RE-404?",
    )
    .await;
    assert_eq!(reply.answer, Some(withheld()), "{reply:?}");
}

#[tokio::test]
async fn an_identifier_a_tool_formatted_from_a_numeric_argument_is_withheld() {
    let reply = lookup_agent_says(
        "withhold",
        vec![
            call("l1", LOOKUP, json!({"id": 999999})),
            text("Your invoice RE-999999 is paid."),
        ],
        "Is my invoice 999999 paid?",
    )
    .await;
    assert_eq!(reply.answer, Some(withheld()), "{reply:?}");
}

#[tokio::test]
async fn an_invoice_a_lookup_by_customer_returned_passes() {
    let reply = lookup_agent_says(
        "withhold",
        vec![
            call("l1", LOOKUP, json!({"customer": "K-12345"})),
            text("RE-123456 is your open invoice."),
        ],
        "Which of my invoices is open? I am K-12345.",
    )
    .await;
    assert_eq!(
        reply.answer.as_deref(),
        Some("RE-123456 is your open invoice."),
        "{reply:?}"
    );
}
