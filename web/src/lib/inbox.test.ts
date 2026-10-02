import test from 'node:test';
import assert from 'node:assert/strict';
import {
	answerFor,
	countFromFrame,
	focused,
	itemHeading,
	itemLink,
	kindLabel,
	minutesLeft,
	prettyArguments,
	slotText,
	type InboxItem
} from './inbox.ts';

function item(over: Partial<InboxItem> = {}): InboxItem {
	return {
		id: 'r1',
		kind: 'human_answer',
		standing: 'responder',
		agent: { id: 'a1', name: 'support', display: 'Support' },
		session_id: 's1',
		turn_id: 't1',
		options: ['value', 'deny'],
		created_at: '2026-10-02T10:00:00Z',
		expires_at: '2026-10-02T10:30:00Z',
		...over
	};
}

test('an inbox frame carries the count, anything else does not', () => {
	assert.equal(countFromFrame('{"type":"inbox","count":3}'), 3);
	assert.equal(countFromFrame('{"type":"inbox","count":-1}'), null);
	assert.equal(countFromFrame('{"type":"other","count":3}'), null);
	assert.equal(countFromFrame('nope'), null);
});

test('a responder gets no link into the agent, a manager and an owner do', () => {
	assert.equal(itemLink(item()), null);
	assert.deepEqual(itemLink(item({ standing: 'manager' }), '/b'), { href: '/b/agents/a1', key: 'inbox-open-agent' });
	assert.deepEqual(itemLink(item({ standing: 'owner', agent: undefined, title: 'Nightly' })), {
		href: '/chat/s1',
		key: 'inbox-open-chat'
	});
});

test('the heading names the agent, or the run', () => {
	assert.deepEqual(itemHeading(item()), { key: 'inbox-item-agent', name: 'Support' });
	assert.deepEqual(itemHeading(item({ agent: undefined, title: 'Nightly' })), { key: 'inbox-item-run', name: 'Nightly' });
	assert.equal(kindLabel('approval'), 'inbox-kind-approval');
	assert.equal(kindLabel('human_answer'), 'inbox-kind-handoff');
});

test('arguments are pretty when they are JSON and kept when they are not', () => {
	assert.equal(prettyArguments('{"a":1}'), '{\n  "a": 1\n}');
	assert.equal(prettyArguments('not json'), 'not json');
	assert.equal(slotText({ slot: 'x', value: 'refund' }), 'refund');
	assert.equal(slotText({ slot: 'x', value: { id: 1 } }), '{"id":1}');
	assert.equal(slotText({ slot: 'x', set_by: 'host' }), null);
});

test('a value answer needs text, the other decisions do not', () => {
	assert.equal(answerFor('value', '  '), null);
	assert.deepEqual(answerFor('value', ' yes '), { decision: 'value', value: 'yes' });
	assert.deepEqual(answerFor('deny', 'ignored'), { decision: 'deny' });
	assert.deepEqual(answerFor('allow_once', ''), { decision: 'allow_once' });
});

test('the deadline counts down and the linked item must still be listed', () => {
	const now = Date.parse('2026-10-02T10:00:00Z');
	assert.equal(minutesLeft('2026-10-02T10:30:00Z', now), 30);
	assert.equal(minutesLeft('2026-10-02T09:00:00Z', now), 0);
	assert.equal(minutesLeft('garbage', now), null);
	assert.equal(focused([item()], new URLSearchParams('item=r1')), 'r1');
	assert.equal(focused([item()], new URLSearchParams('item=gone')), null);
});
