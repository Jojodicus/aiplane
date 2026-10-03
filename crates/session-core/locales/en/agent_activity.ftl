# STATUS: llm-generated, unreviewed — pending native-speaker QA
# Strings owned by the Activity tab of the agent builder (#111): the
# hash-chained activity log, its filters, the export and the chain check.

agents-tab-activity = Activity
agents-act-intro = Everything this agent did, recorded without gaps: every model exchange with the full prompt and answer, every tool call with its full arguments and result, state writes, routing, pauses and every change to the agent. Each conversation is one hash chain, so a change to the log shows up when you verify it. The log holds whole conversations; only people with a share on the agent can read it, and secrets such as one-time codes never enter it.
agents-act-conversation = Conversation
agents-act-conversation-all = All conversations, newest first
agents-act-kind = Show
agents-act-group-all = Everything
agents-act-group-turns = Turns and answers
agents-act-group-exchanges = Model exchanges
agents-act-group-tools = Tool calls
agents-act-group-state = State and verifiers
agents-act-group-routing = Routing and sub-agents
agents-act-group-people = Pauses, people and refusals
agents-act-group-management = Changes to the agent
agents-act-from = From
agents-act-to = To
agents-act-export = Export JSONL
agents-act-verify = Verify chain
agents-act-verified = { $events } events in { $chains } chains are intact.
agents-act-unchained = { $count } older events were recorded before the log was chained.
agents-act-unanchored = { $count } newer events are not anchored yet: they were written after the last turn ended.
agents-act-head = Keep this head of the agent's own chain outside the gateway: it anchors every conversation, so a log cut back to an earlier head shows when you compare.
agents-act-broken = The chain { $chain } breaks at event { $seq }: { $reason }
agents-act-none = No activity matches these filters.
agents-act-more = Load more
agents-act-turn = Turn { $turn }
agents-act-agent-chain = Outside a turn
agents-act-open-conversation = Show this conversation
agents-act-round = round { $round }
agents-act-took = { $ms } ms
agents-act-sub-agent = sub-agent
agents-act-sum-llm = { $model }: { $tokens } tokens, { $finish }
agents-act-sum-llm-error = { $model } failed: { $error }
agents-act-sum-tool = { $tool }: { $status }
agents-act-sum-state = { $slot } written by { $provenance }
agents-act-sum-message = “{ $text }”
agents-act-sum-resumed = Continued after a decision
agents-act-sum-finished = { $status }: { $text }
agents-act-sum-route = { $route }
