/**
 * The prompt assistant's proposal for one agent, kept for the browser tab
 * (`sessionStorage`): a proposal takes half a minute to make, and a reload or
 * a step opened by its URL must not throw it away. Storage can be blocked
 * (private windows, strict settings); then nothing is remembered and the
 * setup still works.
 */
import type { AssistSuggestion } from './agents.ts';

export interface RememberedProposal {
	scenario: string;
	suggestion: AssistSuggestion | null;
	handled: string[];
}

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

const key = (agentId: string) => `aiplane.agent-proposal.${agentId}`;

export function readProposal(storage: Store, agentId: string): RememberedProposal | null {
	try {
		const raw = storage.getItem(key(agentId));
		if (!raw) return null;
		const value: unknown = JSON.parse(raw);
		if (!value || typeof value !== 'object') return null;
		const { scenario, suggestion, handled } = value as Record<string, unknown>;
		if (typeof scenario !== 'string' || !Array.isArray(handled) || !handled.every((h) => typeof h === 'string')) return null;
		if (suggestion !== null && (typeof suggestion !== 'object' || !('steps' in (suggestion as object)))) return null;
		return { scenario, suggestion: suggestion as AssistSuggestion | null, handled: handled as string[] };
	} catch {
		return null;
	}
}

export function writeProposal(storage: Store, agentId: string, value: RememberedProposal): void {
	try {
		if (!value.scenario && !value.suggestion) storage.removeItem(key(agentId));
		else storage.setItem(key(agentId), JSON.stringify(value));
	} catch {
		/* blocked storage: the proposal lives as long as the page */
	}
}

