import assert from 'node:assert/strict';
import { test } from 'node:test';
import { HOLD_MS, nextToSpeak, idleMic, micErrorKey, micStep, type MicEffect, type MicEvent, type MicState } from './voice.ts';

function run(events: MicEvent[], from: MicState = idleMic()): { state: MicState; effects: MicEffect[] } {
	let state = from;
	const effects: MicEffect[] = [];
	for (const event of events) {
		const [next, effect] = micStep(state, event);
		state = next;
		effects.push(effect);
	}
	return { state, effects };
}

test('holding the button records until it is released, then sends', () => {
	const { state, effects } = run([{ type: 'down', at: 0 }, { type: 'started' }, { type: 'up', at: HOLD_MS + 500 }]);
	assert.deepEqual(effects, ['start', null, 'send']);
	assert.equal(state.phase, 'sending');
	assert.equal(run([{ type: 'done' }], state).state.phase, 'idle');
});

test('a short click starts a recording that the next click sends', () => {
	const first = run([{ type: 'down', at: 0 }, { type: 'started' }, { type: 'up', at: 80 }]);
	assert.deepEqual(first.effects, ['start', null, null]);
	assert.equal(first.state.phase, 'recording');
	assert.equal(first.state.held, false);
	const second = run([{ type: 'down', at: 2000 }, { type: 'up', at: 2050 }], first.state);
	assert.deepEqual(second.effects, ['send', null]);
	assert.equal(second.state.phase, 'sending');
});

test('releasing during the permission prompt leaves the recording running until the next click', () => {
	const { state, effects } = run([{ type: 'down', at: 0 }, { type: 'up', at: 3000 }, { type: 'started' }]);
	assert.deepEqual(effects, ['start', null, null]);
	assert.equal(state.phase, 'recording');
	assert.equal(state.held, false);
});

test('the keyboard toggles, cancel discards, and the limit sends', () => {
	assert.deepEqual(run([{ type: 'toggle' }, { type: 'started' }, { type: 'toggle' }]).effects, ['start', null, 'send']);
	const cancelled = run([{ type: 'toggle' }, { type: 'started' }, { type: 'cancel' }]);
	assert.deepEqual(cancelled.effects, ['start', null, 'abort']);
	assert.equal(cancelled.state.phase, 'idle');
	assert.deepEqual(run([{ type: 'toggle' }, { type: 'cancel' }]).effects, ['start', 'abort'], 'cancel while asking');
	assert.deepEqual(run([{ type: 'down', at: 0 }, { type: 'started' }, { type: 'limit' }]).effects, ['start', null, 'send']);
});

test('a refused microphone returns to idle and nothing is sent', () => {
	const { state, effects } = run([{ type: 'down', at: 0 }, { type: 'failed' }, { type: 'up', at: 900 }]);
	assert.deepEqual(effects, ['start', null, null]);
	assert.equal(state.phase, 'idle');
});

test('presses while a recording is sent are ignored', () => {
	const sending: MicState = { phase: 'sending', held: false, pressedAt: 0 };
	assert.deepEqual(run([{ type: 'down', at: 1 }, { type: 'toggle' }, { type: 'cancel' }], sending).effects, [null, null, null]);
});

test('microphone errors map to what the visitor can do about them', () => {
	assert.equal(micErrorKey(new DOMException('no', 'NotAllowedError')), 'embed-voice-denied');
	assert.equal(micErrorKey(new DOMException('none', 'NotFoundError')), 'embed-voice-no-mic');
	assert.equal(micErrorKey(new Error('boom')), 'embed-voice-failed');
});

test('each finished answer is spoken once, whichever frame brought it', () => {
	const msg = (id: string, role = 'assistant', status = 'completed', failed = false) => ({ id, role, status, failed });
	const heard = new Set<string>();
	assert.equal(nextToSpeak([msg('old')], heard), 'old');
	assert.equal(nextToSpeak([msg('old')], heard), null, 'not twice');
	assert.equal(nextToSpeak([msg('old'), msg('u1', 'user'), msg('a1', 'assistant', 'in_progress')], heard), null);
	assert.equal(nextToSpeak([msg('old'), msg('u1', 'user'), msg('a1')], heard), 'a1');
	assert.equal(nextToSpeak([msg('e1', 'assistant', 'errored', true)], heard), null, 'an error is not read');
});
