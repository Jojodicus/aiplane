import test from 'node:test';
import assert from 'node:assert/strict';

import { filterTokenModels, groupTokenModels, isCompliant, selectionSummary, type TokenModel } from './token-models.ts';

function model(id: string, overrides: Partial<TokenModel> = {}): TokenModel {
	return { id, kind: 'chat', gdpr: true, nda: true, alias_of: null, price: null, ...overrides };
}

const qwen = model('qwen');
const kimi = model('kimi', { gdpr: false, nda: false, price: { input: 0.6, output: 2.5, unit: 'tokens' } });
const glm = model('glm', { nda: false, price: { input: 0.3, output: 1.2, unit: 'tokens' } });
const bge = model('bge', { kind: 'embedding' });
const image = model('gpt-image-2', { kind: 'image', gdpr: false, price: { input: null, output: 0.04, unit: 'images' } });

test('models group by kind in a fixed order, so chat models come first', () => {
	const groups = groupTokenModels([image, bge, qwen]);
	assert.deepEqual(groups.map((group) => group.kind), ['chat', 'embedding', 'image']);
	assert.deepEqual(groups[0].models, [qwen]);
});

test('a kind the SPA does not know yet still shows, after the known ones', () => {
	const odd = model('odd', { kind: 'future' });
	assert.deepEqual(groupTokenModels([odd, qwen]).map((group) => group.kind), ['chat', 'future']);
});

test('a model is compliant only when both GDPR and NDA hold', () => {
	assert.equal(isCompliant(qwen), true);
	assert.equal(isCompliant(glm), false);
	assert.equal(isCompliant(kimi), false);
});

test('filters narrow by name, by data handling and by price together', () => {
	const all = [qwen, kimi, glm, bge, image];
	assert.deepEqual(filterTokenModels(all, { query: 'KI', gdpr: false, nda: false, free: false }), [kimi]);
	assert.deepEqual(filterTokenModels(all, { query: '', gdpr: true, nda: false, free: false }), [qwen, glm, bge]);
	assert.deepEqual(filterTokenModels(all, { query: '', gdpr: true, nda: true, free: false }), [qwen, bge]);
	assert.deepEqual(filterTokenModels(all, { query: '', gdpr: false, nda: false, free: true }), [qwen, bge]);
});

test('the summary counts what leaves the protection boundary and the dearest token price', () => {
	const all = [qwen, kimi, glm, bge, image];
	assert.deepEqual(selectionSummary(all, ['qwen', 'bge']), { count: 2, nonCompliant: 0, maxOutputPerMillion: null });
	assert.deepEqual(selectionSummary(all, ['qwen', 'kimi', 'glm', 'gpt-image-2']), { count: 4, nonCompliant: 3, maxOutputPerMillion: 2.5 });
});

test('the summary of an unrestricted token covers every model it can reach', () => {
	assert.deepEqual(selectionSummary([qwen, kimi], null), { count: 2, nonCompliant: 1, maxOutputPerMillion: 2.5 });
});

test('a selected model that has since disappeared is not counted', () => {
	assert.deepEqual(selectionSummary([qwen], ['qwen', 'gone']), { count: 1, nonCompliant: 0, maxOutputPerMillion: null });
});
