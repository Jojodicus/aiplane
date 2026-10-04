export const MEMORY_KINDS = ['preference', 'project', 'fact'] as const;

export type MemoryKind = (typeof MEMORY_KINDS)[number];

export interface Memory {
	id: string;
	kind: MemoryKind;
	content: string;
	created_at: string;
}

/** Whether preferences reach the assistant, and the limits that apply when they do. */
export interface PreferenceContext {
	in_context: boolean;
	max_count: number;
	char_budget: number;
}

export function groupMemories(memories: Memory[]): Record<MemoryKind, Memory[]> {
	return {
		preference: memories.filter((memory) => memory.kind === 'preference'),
		project: memories.filter((memory) => memory.kind === 'project'),
		fact: memories.filter((memory) => memory.kind === 'fact')
	};
}
