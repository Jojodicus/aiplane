import assert from 'node:assert/strict';
import test from 'node:test';

import type { AssistSuggestion } from './agents.ts';
import { readProposal, writeProposal } from './proposal-memory.ts';

class MemoryStorage {
	items = new Map<string, string>();
	getItem(key: string) {
		return this.items.get(key) ?? null;
	}
	setItem(key: string, value: string) {
		this.items.set(key, value);
	}
	removeItem(key: string) {
		this.items.delete(key);
	}
}

const suggestion = { steps: { task: 'Help visitors.' }, dropped: [] } as unknown as AssistSuggestion;

test('a proposal survives a reload of the same agent only', () => {
	const storage = new MemoryStorage();
	writeProposal(storage, 'a1', { scenario: 'A sales bot', suggestion, handled: ['task'] });
	assert.deepEqual(readProposal(storage, 'a1'), { scenario: 'A sales bot', suggestion, handled: ['task'] });
	assert.equal(readProposal(storage, 'a2'), null);
});

test('nothing left to remember removes the entry', () => {
	const storage = new MemoryStorage();
	writeProposal(storage, 'a1', { scenario: '', suggestion: null, handled: [] });
	assert.equal(storage.items.size, 0);
	writeProposal(storage, 'a1', { scenario: 'x', suggestion: null, handled: [] });
	writeProposal(storage, 'a1', { scenario: '', suggestion: null, handled: [] });
	assert.equal(readProposal(storage, 'a1'), null);
});

test('a damaged entry or a refusing storage reads as nothing', () => {
	const storage = new MemoryStorage();
	storage.setItem('aiplane.agent-proposal.a1', '{not json');
	assert.equal(readProposal(storage, 'a1'), null);
	storage.setItem('aiplane.agent-proposal.a1', JSON.stringify({ scenario: 3 }));
	assert.equal(readProposal(storage, 'a1'), null);
	const refusing = {
		getItem: () => {
			throw new Error('blocked');
		},
		setItem: () => {
			throw new Error('blocked');
		},
		removeItem: () => {
			throw new Error('blocked');
		}
	};
	assert.equal(readProposal(refusing, 'a1'), null);
	assert.doesNotThrow(() => writeProposal(refusing, 'a1', { scenario: 'x', suggestion: null, handled: [] }));
});
