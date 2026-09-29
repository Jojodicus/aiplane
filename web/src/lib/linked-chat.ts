import type { ChatSession } from './chat-protocol';
import type { SearchOption } from './searchable-select';

/**
 * Where a reusing schedule or webhook writes its next run: one of the owner's
 * chats, or (the empty value) a fresh chat the next run opens.
 */

export interface LinkedChatRow {
	last_session_id: string | null;
	last_chat_deleted?: boolean;
}

/**
 * The chat the form starts on. That is `last_session_id` whether or not reuse
 * is on yet, because it is exactly where a reusing run would continue — so
 * turning reuse on shows the truth rather than "a new chat". A deleted one is
 * not offered: the next run opens a fresh chat, and that is what the form says.
 */
export function initialLinkedSession(row: LinkedChatRow | null): string {
	return row?.last_session_id && !row.last_chat_deleted ? row.last_session_id : '';
}

export function linkedChatOptions(
	sessions: Pick<ChatSession, 'id' | 'title'>[],
	labels: { fresh: string; untitled: string }
): SearchOption[] {
	return [
		{ value: '', label: labels.fresh },
		...sessions.map((session) => ({
			value: session.id,
			label: session.title?.trim() || labels.untitled,
			keywords: [session.id]
		}))
	];
}

/**
 * The save body's `linked_session_id`: sent only when reuse is on and the
 * choice changed, because an absent field is how the server knows to leave
 * the link alone — a run may have moved it since the form was opened.
 */
export function linkedSessionField(reuse: boolean, initial: string, current: string): { linked_session_id?: string } {
	return reuse && current !== initial ? { linked_session_id: current } : {};
}
