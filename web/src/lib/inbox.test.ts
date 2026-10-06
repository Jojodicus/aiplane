import test from 'node:test';
import assert from 'node:assert/strict';
import { focused, frameOf, inboxShown, itemHeading, itemLink, kindLabel, type InboxItem } from './inbox.ts';

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

test('an inbox frame carries the count and whether the viewer answers for an agent, anything else does not', () => {
	assert.deepEqual(frameOf('{"type":"inbox","count":3,"answers":true}'), { count: 3, answers: true });
	assert.deepEqual(frameOf('{"type":"inbox","count":0}'), { count: 0, answers: false });
	assert.equal(frameOf('{"type":"inbox","count":-1}'), null);
	assert.equal(frameOf('{"type":"other","count":3}'), null);
	assert.equal(frameOf('nope'), null);
});

test('the inbox is shown when something waits, when the viewer answers for an agent, or while it is open', () => {
	assert.equal(inboxShown({ count: 0, answers: false }, false), false);
	assert.equal(inboxShown({ count: 2, answers: false }, false), true);
	assert.equal(inboxShown({ count: 0, answers: true }, false), true);
	assert.equal(inboxShown({ count: 0, answers: false }, true), true);
});

test('a responder gets no link into the agent, a manager and an owner do', () => {
	assert.equal(itemLink(item()), null);
	assert.deepEqual(itemLink(item({ standing: 'manager' })), { href: '/agents/a1', key: 'inbox-open-agent' });
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

test('the linked item must still be listed', () => {
	assert.equal(focused([item()], new URLSearchParams('item=r1')), 'r1');
	assert.equal(focused([item()], new URLSearchParams('item=gone')), null);
});
