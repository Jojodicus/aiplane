import assert from 'node:assert/strict';
import { test } from 'node:test';
import { trackWaiting, waitingFromTurns, waitingLabel } from './waiting.ts';

const suspendedTurn = (options: string[], kind = 'human_answer') => ({
	turn: { id: 'a1', role: 'assistant', status: 'suspended' },
	suspension: { request_id: 'r1', kind, options, expires_at: '2026-10-02T10:30:00Z' }
});

test('a pause that offers the visitor nothing is a wait for staff', () => {
	assert.deepEqual(waitingFromTurns([suspendedTurn([])]), { kind: 'human_answer', turnId: 'a1' });
	assert.equal(waitingFromTurns([suspendedTurn(['value', 'deny'], 'secure_input')]), null);
	assert.equal(waitingFromTurns([{ turn: { id: 'a1', status: 'completed' } }]), null);
});

test('frames start and end the wait', () => {
	const waiting = trackWaiting(null, {
		event: 'suspended',
		data: { type: 'suspended', turn_id: 'a1', request_id: 'r1', kind: 'approval', options: [] }
	});
	assert.deepEqual(waiting, { kind: 'approval', turnId: 'a1' });
	assert.equal(waitingLabel(waiting!), 'embed-waiting-approval');
	assert.equal(trackWaiting(waiting, { event: 'idle', data: {} }), waiting);
	assert.equal(trackWaiting(waiting, { event: 'turn_finalized', data: { turn_id: 'a1', status: 'completed' } }), null);
	assert.deepEqual(trackWaiting(null, { event: 'snapshot', data: { turns: [suspendedTurn([])] } }), {
		kind: 'human_answer',
		turnId: 'a1'
	});
	assert.equal(trackWaiting(waiting, { event: 'snapshot', data: { turns: [] } }), null);
	assert.equal(
		trackWaiting(null, { event: 'suspended', data: { turn_id: 'a1', kind: 'secure_input', options: ['value', 'deny'] } }),
		null
	);
});
