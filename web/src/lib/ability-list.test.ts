import assert from 'node:assert/strict';
import test from 'node:test';

import type { Ability } from './agent-setup.ts';
import { abilityTitle, filterAbilities, orderAbilities, plainText, visibleAbilities } from './ability-list.ts';

const card = (ref: string, { name, description, ...over }: Partial<Ability> & { name?: string; description?: string } = {}): Ability => ({
	kind: 'tool',
	ref,
	refs: [ref],
	tools: [ref],
	on: false,
	holdable: true,
	item: { key: ref, kind: 'tool', title: name ?? ref, description: description ?? '', group: 'g', order: 0, icon: null, grant: { kind: 'tool', refs: [ref] }, tools: [ref], editable: false, config_url: null },
	...over
});
const refs = (list: Ability[]) => list.map((c) => c.ref);

test('on first, then suggested, then the rest by title', () => {
	const list = [card('zeta'), card('beta'), card('alpha', { on: true }), card('gamma'), card('omega', { on: true })];
	assert.deepEqual(refs(orderAbilities(list, ['gamma'])), ['alpha', 'omega', 'gamma', 'beta', 'zeta']);
});

test('a suggested card that is already on stays in the on group', () => {
	const list = [card('b'), card('a', { on: true })];
	assert.deepEqual(refs(orderAbilities(list, ['a'])), ['a', 'b']);
});

test('titles sort by the shown name', () => {
	const list = [card('b_tool', { name: 'Zebra' }), card('a_tool', { name: 'apple' })];
	assert.deepEqual(refs(orderAbilities(list, [])), ['a_tool', 'b_tool']);
	assert.notEqual(abilityTitle(card('fetch_url')), 'fetch_url');
});

test('filter matches title and description, case-insensitively', () => {
	const list = [card('a', { name: 'Search web', description: 'Looks things up' }), card('b', { name: 'Calendar' })];
	assert.deepEqual(refs(filterAbilities(list, 'WEB')), ['a']);
	assert.deepEqual(refs(filterAbilities(list, 'looks')), ['a']);
	assert.deepEqual(refs(filterAbilities(list, '  ')), ['a', 'b']);
});

test('collapsed list shows only what is on or suggested', () => {
	const many = Array.from({ length: 20 }, (_, i) => card(`t${String(i).padStart(2, '0')}`));
	many[10] = card('t10', { on: true });
	const ordered = orderAbilities(many, ['t15']);
	const shown = visibleAbilities(ordered, ['t15'], false);
	assert.deepEqual(refs(shown).slice(0, 2), ['t10', 't15']);
	assert.equal(shown.length, 2);
	assert.equal(visibleAbilities(ordered, ['t15'], true).length, 20);
});

test('plainText drops markdown backticks', () => {
	assert.equal(plainText('Inspect `typst_onepager` output'), 'Inspect typst_onepager output');
});
