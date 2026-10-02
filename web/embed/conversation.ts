/**
 * The widget's conversation state, free of the DOM: what the transcript holds
 * and how `chat_json` frames change it. Public answers arrive whole (`turn_delta`
 * with `full: true`), so there is no partial text to reconcile.
 */
import type { Frame } from './frames.ts';
import type { TurnView, TurnWithTools } from './api.ts';

export interface Message {
	id: string;
	role: 'user' | 'assistant';
	text: string;
	status: TurnView['status'];
	failed: boolean;
}

export interface Conversation {
	messages: Message[];
	/** An answer is still being produced (and, for public agents, held back until it is whole). */
	pending: boolean;
}

export function emptyConversation(): Conversation {
	return { messages: [], pending: false };
}

export function fromTurns(turns: TurnWithTools[], liveTurnId: string | null): Conversation {
	const messages: Message[] = [];
	for (const { turn } of turns) {
		const text = turn.role === 'user' ? turn.user_content : turn.content;
		const failed = turn.status === 'errored';
		if (turn.role === 'assistant' && !text && !failed) continue;
		messages.push({ id: turn.id, role: turn.role, text: text ?? '', status: turn.status, failed });
	}
	return { messages, pending: liveTurnId !== null };
}

/** What a frame did, beyond the state change, that the view has to react to. */
export type Outcome = 'none' | 'finished' | 'idle';

export function applyFrame(state: Conversation, frame: Frame): Outcome {
	const data = frame.data;
	switch (frame.event) {
		case 'snapshot': {
			const next = fromTurns((data.turns as TurnWithTools[] | undefined) ?? [], (data.live_turn_id as string | null | undefined) ?? null);
			state.messages = next.messages;
			state.pending = next.pending;
			return 'none';
		}
		case 'turn_delta': {
			const id = String(data.turn_id);
			const delta = String(data.text_delta ?? '');
			const existing = state.messages.find((m) => m.id === id);
			if (!existing) {
				state.messages.push({ id, role: 'assistant', text: delta, status: 'in_progress', failed: false });
			} else {
				existing.text = data.full === true ? delta : existing.text + delta;
			}
			return 'none';
		}
		case 'turn_finalized': {
			const id = String(data.turn_id);
			const status = String(data.status) as Message['status'];
			const failed = status === 'errored';
			let message = state.messages.find((m) => m.id === id);
			if (!message && failed) {
				message = { id, role: 'assistant', text: '', status, failed };
				state.messages.push(message);
			}
			if (message) {
				message.status = status;
				message.failed = failed;
				if (failed && typeof data.error_message === 'string') message.text = data.error_message;
			}
			state.pending = false;
			return 'finished';
		}
		case 'idle':
			state.pending = false;
			return 'idle';
		default:
			return 'none';
	}
}
