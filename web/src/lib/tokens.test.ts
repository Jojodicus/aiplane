import assert from 'node:assert/strict';
import test from 'node:test';

import { daysUntilExpiry, quotaChanges, quotaUsed, tokenDate, type TokenQuota } from './tokens.ts';

test('tokenDate uses the account timezone without locale-dependent punctuation', () => {
	assert.equal(tokenDate('2026-09-05T23:30:00Z', 'Asia/Tokyo'), '2026-09-06');
});

test('tokenDate leaves an unexpected value visible', () => {
	assert.equal(tokenDate('unknown', 'UTC'), 'unknown');
});

test('expiry warns within two weeks and stays quiet before', () => {
	const now = Date.parse('2026-10-01T00:00:00Z');
	assert.equal(daysUntilExpiry('2026-10-06T00:00:00Z', now), 5);
	assert.equal(daysUntilExpiry('2026-10-01T10:00:00Z', now), 0, 'less than a day left reads as today');
	assert.equal(daysUntilExpiry('2027-10-01T00:00:00Z', now), null);
});

test('a token past its expiry is expired, not expiring today', () => {
	const now = Date.parse('2026-10-01T00:00:00Z');
	assert.equal(daysUntilExpiry('2026-09-30T00:00:00Z', now), 'expired');
	assert.equal(daysUntilExpiry('2026-10-01T00:00:00Z', now), 'expired');
});

const ownerRule: TokenQuota = { id: 'q1', model: null, dimension: 'cost', window: 'month', value: 20, managed_by: 'owner' };
const adminRule: TokenQuota = { id: 'q2', model: null, dimension: 'requests', window: 'day', value: 1000, managed_by: 'admin' };

test('saving the budget removes only the owner rules that were dropped', () => {
	const added = [{ dimension: 'tokens' as const, window: 'day' as const, value: 5 }];
	assert.deepEqual(quotaChanges([ownerRule, adminRule], [], added), { remove: ['q1'], add: added });
	assert.deepEqual(quotaChanges([ownerRule, adminRule], ['q1'], []), { remove: [], add: [] });
});

test('a rule reads its spend from the status of the same limit', () => {
	const status = [{ model: null, dimension: 'cost', window: 'month', limit: 20, used: 12.4, refreshes_at: '' }];
	assert.equal(quotaUsed(ownerRule, status), 12.4);
	assert.equal(quotaUsed(adminRule, status), null);
});
