import assert from 'node:assert/strict';
import test from 'node:test';

import { initialLinkedSession, linkedChatOptions, linkedSessionField } from './linked-chat.ts';

test('the form starts on the chat a reusing run would continue in', () => {
	assert.equal(initialLinkedSession({ last_session_id: 'sess-1' }), 'sess-1');
	assert.equal(initialLinkedSession(null), '');
	assert.equal(initialLinkedSession({ last_session_id: null }), '');
});

test('a deleted chat is not offered: the next run opens a fresh one', () => {
	assert.equal(initialLinkedSession({ last_session_id: 'sess-1', last_chat_deleted: true }), '');
});

test('the choices are a fresh chat first, then the owner\'s chats', () => {
	const options = linkedChatOptions(
		[
			{ id: 'a', title: 'Boat search' },
			{ id: 'b', title: null }
		],
		{ fresh: 'New chat', untitled: 'Untitled chat' }
	);
	assert.deepEqual(
		options.map((option) => [option.value, option.label]),
		[
			['', 'New chat'],
			['a', 'Boat search'],
			['b', 'Untitled chat']
		]
	);
});

test('the link is sent only when reuse is on and the choice changed', () => {
	assert.deepEqual(linkedSessionField(true, 'a', 'b'), { linked_session_id: 'b' });
	assert.deepEqual(linkedSessionField(true, 'a', ''), { linked_session_id: '' });
	assert.deepEqual(linkedSessionField(true, 'a', 'a'), {});
	assert.deepEqual(linkedSessionField(false, 'a', 'b'), {});
});
