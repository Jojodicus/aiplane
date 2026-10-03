import assert from 'node:assert/strict';
import test from 'node:test';

import type { Grant } from './agents.ts';
import { emptyPlan, isEmpty, plannedGrants, stageGrant, stageRevoke } from './agent-grant-plan.ts';

const g = (kind: Grant['kind'], ref: string): Grant => ({ kind, ref, granted_by: 'admin', granted_at: '' });
const held = [g('pool', 'chat'), g('tool', 'search_web')];

test('staging a grant does nothing for one already held, and adds it once otherwise', () => {
	let plan = stageGrant(emptyPlan(), held, { kind: 'pool', ref: 'chat' });
	assert.ok(isEmpty(plan));
	plan = stageGrant(plan, held, { kind: 'tool', ref: 'rag_search' });
	plan = stageGrant(plan, held, { kind: 'tool', ref: 'rag_search' });
	assert.deepEqual(plan, { grant: [{ kind: 'tool', ref: 'rag_search' }], revoke: [] });
	assert.deepEqual(plannedGrants(held, plan).map((x) => x.ref), ['chat', 'search_web', 'rag_search']);
});

test('switching off a staged grant withdraws it instead of revoking anything', () => {
	const staged = stageGrant(emptyPlan(), held, { kind: 'pool', ref: 'large' });
	const { plan, kept } = stageRevoke(staged, held, null, { kind: 'pool', ref: 'large' });
	assert.ok(isEmpty(plan));
	assert.equal(kept, false);
});

test('a held grant is revoked on save, unless the published version uses it', () => {
	const { plan } = stageRevoke(emptyPlan(), held, null, { kind: 'tool', ref: 'search_web' });
	assert.deepEqual(plan.revoke, [{ kind: 'tool', ref: 'search_web' }]);
	assert.deepEqual(plannedGrants(held, plan).map((x) => x.ref), ['chat']);

	const live = { main: { pool: 'chat', tools: ['search_web'] } };
	const keep = stageRevoke(emptyPlan(), held, live, { kind: 'tool', ref: 'search_web' });
	assert.ok(isEmpty(keep.plan));
	assert.equal(keep.kept, true);

	const back = stageGrant(plan, held, { kind: 'tool', ref: 'search_web' });
	assert.ok(isEmpty(back), 'switching it on again withdraws the revocation');
});

test('nothing to revoke for what the agent never held', () => {
	const { plan, kept } = stageRevoke(emptyPlan(), held, null, { kind: 'skill', ref: 'x' });
	assert.ok(isEmpty(plan));
	assert.equal(kept, false);
});
