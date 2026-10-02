import assert from 'node:assert/strict';
import test from 'node:test';

import { analyticsQuery, bars, countRows, type DayBucket } from './agent-analytics.ts';

const day = (d: string, over: Partial<DayBucket> = {}): DayBucket => ({
	day: d,
	conversations: 0,
	turns: 0,
	tokens: 0,
	cost: 0,
	refusals: 0,
	...over
});

test('the query spans the last N UTC days including today', () => {
	const now = new Date('2026-09-30T23:30:00Z');
	assert.equal(analyticsQuery(7, null, now), 'from=2026-09-24&to=2026-09-30');
	assert.equal(analyticsQuery(1, 3, now), 'from=2026-09-30&to=2026-09-30&version=3');
});

test('count rows are ordered by count, then name', () => {
	assert.deepEqual(countRows({ b: 1, a: 1, c: 5 }), [
		['c', 5],
		['a', 1],
		['b', 1]
	]);
	assert.deepEqual(countRows({}), []);
});

test('bars scale to the busiest day and keep a quiet day visible', () => {
	const out = bars(
		[day('1', { turns: 100 }), day('2', { turns: 1 }), day('3')],
		'turns',
		300,
		50
	);
	assert.equal(out[0].height, 50);
	assert.equal(out[0].y, 0);
	assert.equal(out[1].height, 1);
	assert.equal(out[2].height, 0);
	assert.ok(out[0].x < out[1].x && out[1].x < out[2].x);
});

test('bars of an all-zero metric are flat and no bars come from no days', () => {
	assert.deepEqual(
		bars([day('1'), day('2')], 'cost', 100, 20).map((b) => b.height),
		[0, 0]
	);
	assert.deepEqual(bars([], 'cost', 100, 20), []);
});
