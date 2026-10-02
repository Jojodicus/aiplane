import assert from 'node:assert/strict';
import { test } from 'node:test';
import { codeToSend, secureRequest } from './secure-input.ts';
import type { Waiting } from './api.ts';

const request = (over: Partial<Waiting>): Waiting => ({
	request_id: 'req-1',
	kind: 'secure_input',
	message: 'Enter the code.',
	options: ['value', 'deny'],
	expires_at: '2026-10-02T12:10:00Z',
	...over
});

test('the field answers only a secure input the visitor may give', () => {
	assert.equal(secureRequest(request({}))?.request_id, 'req-1');
	assert.equal(secureRequest(null), null);
	assert.equal(secureRequest(request({ kind: 'approval', options: [] })), null, 'an approval is for staff');
	assert.equal(secureRequest(request({ kind: 'human_answer', options: [] })), null);
	assert.equal(secureRequest(request({ options: ['deny'] })), null);
});

test('a code is sent trimmed, and an empty one not at all', () => {
	assert.equal(codeToSend(' 481516 \n'), '481516');
	assert.equal(codeToSend('   '), null);
	assert.equal(codeToSend(''), null);
});
