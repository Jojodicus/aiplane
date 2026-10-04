import assert from 'node:assert/strict';
import test from 'node:test';

import type { ToolCall } from './chat-protocol.ts';
import { changesAgent, conversationTitle, setupPath, toolLabel, undoTarget } from './architect.ts';

const call = (name: string, output: unknown, status: ToolCall['status'] = 'completed'): ToolCall => ({
	id: `${name}-1`,
	turn_id: 't',
	seq: 0,
	name,
	arguments_json: '{}',
	output_json: output === null ? null : JSON.stringify(output),
	status,
	created_at: '',
	completed_at: null
});

test('a completed draft change can be undone to the revision it kept', () => {
	const done = call('update_agent_draft', { agent_id: 'a1', revision: 7, setup_url: '/agents/a1' });
	assert.deepEqual(undoTarget(done), { agentId: 'a1', revision: 7 });
	assert.equal(undoTarget(call('update_agent_draft', { agent_id: 'a1', revision: null })), null, 'nothing changed');
	assert.equal(undoTarget(call('update_agent_draft', null, 'errored')), null);
	assert.equal(undoTarget(call('read_agent', { agent_id: 'a1', revision: 3 })), null);
	assert.equal(undoTarget({ ...done, output_json: 'not json' }), null);
});

test('only an app path from a call that names a setup page becomes a link', () => {
	assert.equal(setupPath(call('create_agent_draft', { setup_url: '/agents/a1' })), '/agents/a1');
	assert.equal(setupPath(call('create_agent_draft', { setup_url: 'https://evil.example/' })), null);
	assert.equal(setupPath(call('list_agents', { setup_url: '/agents/a1' })), null);
	assert.equal(setupPath(call('create_agent_draft', null, 'running')), null);
});

test('a page reloads after a create or a draft change, not after a read', () => {
	assert.ok(changesAgent(call('create_agent_draft', {})));
	assert.ok(changesAgent(call('update_agent_draft', {})));
	assert.ok(!changesAgent(call('update_agent_draft', null, 'errored')));
	assert.ok(!changesAgent(call('read_agent', {})));
});

test('the conversation is titled after the agent, or as a new one', () => {
	const tr = (key: string, args?: Record<string, string | number>) => `${key}${args ? `:${String(args.name)}` : ''}`;
	assert.equal(conversationTitle(tr, ' Harald '), 'architect-conversation-title:Harald');
	assert.equal(conversationTitle(tr, ''), 'architect-conversation-new');
	assert.equal(conversationTitle(tr, null), 'architect-conversation-new');
});

test('an architect tool reads as what it did, any other keeps its name', () => {
	const tr = (key: string) => `<${key}>`;
	assert.equal(toolLabel(tr)('run_test_turn'), '<architect-tool-run-test-turn>');
	assert.equal(toolLabel(tr)('list_grantable'), '<architect-tool-list-grantable>');
	assert.equal(toolLabel(tr)('fetch_url'), 'fetch_url');
});
