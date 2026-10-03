import test from 'node:test';
import assert from 'node:assert';
import { chipClass, choiceCardClass, segmentClass, statusPillClass, stepClass, stepState } from './ui-variants.ts';

test('a status pill is a soft, fully rounded badge in its tone colour', () => {
	assert.equal(statusPillClass('ok'), 'badge badge-soft rounded-full font-semibold badge-success');
	assert.equal(statusPillClass('warn'), 'badge badge-soft rounded-full font-semibold badge-warning');
	assert.equal(statusPillClass('bad'), 'badge badge-soft rounded-full font-semibold badge-error');
	assert.equal(statusPillClass('info'), 'badge badge-soft rounded-full font-semibold badge-info');
	assert.equal(statusPillClass('neutral'), 'badge badge-soft rounded-full font-semibold text-base-content/70');
});

test('the current step wins over done, and a step is upcoming otherwise', () => {
	assert.equal(stepState(1, 1, [0, 1]), 'current');
	assert.equal(stepState(0, 1, [0]), 'done');
	assert.equal(stepState(2, 1, [0]), 'upcoming');
});

test('each step state has its own pill', () => {
	assert.match(stepClass('current'), /btn-primary/);
	assert.match(stepClass('done'), /text-success/);
	assert.match(stepClass('upcoming'), /text-base-content\/60/);
	for (const state of ['current', 'done', 'upcoming'] as const) assert.match(stepClass(state), /^btn btn-xs rounded-full/);
});

test('selected chips, choice cards and segments carry the primary colour; idle ones do not', () => {
	for (const variant of [chipClass, choiceCardClass, segmentClass]) {
		assert.match(variant(true), /primary/);
		assert.doesNotMatch(variant(false), /primary/);
	}
	assert.match(choiceCardClass(false), /border-base-300/);
	assert.match(segmentClass(false), /^btn join-item/);
});
