import assert from 'node:assert/strict';
import { test } from 'node:test';
import { applyFrame, emptyConversation, fromTurns } from './conversation.ts';
import type { TurnView, TurnWithTools } from './api.ts';

const turn = (over: Partial<TurnView>): TurnWithTools => ({ turn: {
	id: 't',
	role: 'assistant',
	user_content: null,
	content: null,
	status: 'completed',
	error_message: null,
	...over
} });

test('a snapshot with a running turn is pending and hides the empty answer', () => {
	const state = fromTurns(
		[
			turn({ id: 'u1', role: 'user', user_content: 'hi' }),
			turn({ id: 'a1', status: 'in_progress' })
		],
		'a1'
	);
	assert.equal(state.pending, true);
	assert.deepEqual(
		state.messages.map((m) => [m.role, m.text]),
		[['user', 'hi']]
	);
});

test('a whole answer replaces, and the finalize ends the wait', () => {
	const state = fromTurns([turn({ id: 'u1', role: 'user', user_content: 'hi' })], 'a1');
	assert.equal(applyFrame(state, { event: 'turn_delta', data: { turn_id: 'a1', text_delta: 'Hello!', full: true } }), 'none');
	assert.equal(state.messages.at(-1)?.text, 'Hello!');
	assert.equal(state.pending, true);
	assert.equal(applyFrame(state, { event: 'turn_finalized', data: { turn_id: 'a1', status: 'completed' } }), 'finished');
	assert.equal(state.pending, false);
	assert.equal(state.messages.at(-1)?.status, 'completed');
});

test('a non-full delta appends and a full one rewrites', () => {
	const state = emptyConversation();
	applyFrame(state, { event: 'turn_delta', data: { turn_id: 'a', text_delta: 'Hel' } });
	applyFrame(state, { event: 'turn_delta', data: { turn_id: 'a', text_delta: 'lo' } });
	assert.equal(state.messages[0].text, 'Hello');
	applyFrame(state, { event: 'turn_delta', data: { turn_id: 'a', text_delta: 'Bye', full: true } });
	assert.equal(state.messages[0].text, 'Bye');
});

test('an errored turn shows the gateway message, even with no content', () => {
	const state = fromTurns([turn({ id: 'u1', role: 'user', user_content: 'hi' })], 'a1');
	applyFrame(state, {
		event: 'turn_finalized',
		data: { turn_id: 'a1', status: 'errored', error_message: 'The assistant could not answer.' }
	});
	const last = state.messages.at(-1);
	assert.equal(last?.failed, true);
	assert.equal(last?.text, 'The assistant could not answer.');
});

test('idle clears pending and unknown events change nothing', () => {
	const state = emptyConversation();
	state.pending = true;
	assert.equal(applyFrame(state, { event: 'tool_call_started', data: {} }), 'none');
	assert.equal(state.pending, true);
	assert.equal(applyFrame(state, { event: 'idle', data: {} }), 'idle');
	assert.equal(state.pending, false);
});

test('a snapshot rebuilds the transcript from the server, replacing local state', () => {
	const state = emptyConversation();
	state.messages.push({ id: 'local', role: 'user', text: 'x', status: 'completed', failed: false });
	applyFrame(state, {
		event: 'snapshot',
		data: { turns: [turn({ id: 'u9', role: 'user', user_content: 'server' })], live_turn_id: null }
	});
	assert.deepEqual(state.messages.map((m) => m.id), ['u9']);
});
