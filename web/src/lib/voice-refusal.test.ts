import assert from 'node:assert/strict';
import test from 'node:test';

import { ApiError } from './api.ts';
import { isLimitRefusal, voiceRefusalMessage } from './voice-refusal.ts';

const tr = (key: string, args?: Record<string, string | number>) =>
	args ? `t:${key}:${JSON.stringify(args)}` : `t:${key}`;

const refused = (status: number, body: string) => new ApiError(status, `${status}`, undefined, undefined, body);
const envelope = (code: string, message: string) => JSON.stringify({ error: { code, message } });

test('the gateway refusing a user over budget is worded from the catalog', () => {
	for (const code of ['rate_limited', 'rate_limit_exceeded']) {
		const err = refused(429, envelope(code, 'rate limit or quota exceeded — see /usage'));
		assert.equal(isLimitRefusal(err), true, code);
		assert.equal(voiceRefusalMessage(err, tr), 't:voice-limit-reached', code);
	}
});

test('a 429 the gateway did not send as a limit keeps its own words', () => {
	const err = refused(429, envelope('upstream_busy', 'The speech backend is busy.'));
	assert.equal(isLimitRefusal(err), false);
	assert.equal(voiceRefusalMessage(err, tr), 'The speech backend is busy.');
});

test('a 429 without an envelope is not a limit and still says something', () => {
	const err = refused(429, 'Too Many Requests');
	assert.equal(isLimitRefusal(err), false);
	assert.equal(voiceRefusalMessage(err, tr), 't:error-request-failed:{"status":429}');
});

test('any other refusal keeps the message of its error envelope', () => {
	const err = refused(400, envelope('audio_too_short', 'Recording too short.'));
	assert.equal(voiceRefusalMessage(err, tr), 'Recording too short.');
});

test('an empty or unparsable body falls back to a message naming the status', () => {
	assert.equal(voiceRefusalMessage(refused(502, ''), tr), 't:error-request-failed:{"status":502}');
	assert.equal(voiceRefusalMessage(refused(502, '<html>bad gateway</html>'), tr), 't:error-request-failed:{"status":502}');
	assert.equal(voiceRefusalMessage(refused(500, envelope('internal_error', '  ')), tr), 't:error-request-failed:{"status":500}');
});

test('a long server sentence is capped', () => {
	assert.equal(voiceRefusalMessage(refused(400, envelope('x', 'y'.repeat(500))), tr).length, 200);
});
