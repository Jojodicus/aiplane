/**
 * The human-in-the-loop inbox (`docs/agents.md` "What #96 built"): wire
 * types, the calls, and the pure helpers the page leans on.
 *
 * An item is something a paused turn waits for: an approval of a tool call
 * or a visitor's request for a person, in an agent conversation, or a
 * person's own scheduled or webhook run that paused. Who sees it is the
 * server's decision; `standing` says why the viewer does, and decides which
 * links the page may offer — a responder sees the item and nothing of the
 * agent behind it.
 */
import { ApiError, request } from './api.ts';

export type InboxKind = 'approval' | 'human_answer';
export type Standing = 'manager' | 'responder' | 'owner';
export type DecisionKind = 'allow_once' | 'deny' | 'value';

export interface InboxSlot {
	slot: string;
	value?: unknown;
	set_by?: string;
}

export interface InboxContext {
	route?: string;
	question?: string;
	visitor_message?: string | null;
	slots?: InboxSlot[];
	lang?: string;
	inbox?: string | null;
	notify?: string[] | null;
	transcript?: { role: 'user' | 'assistant'; text: string }[];
}

export interface InboxItem {
	id: string;
	kind: InboxKind;
	standing: Standing;
	agent?: { id: string; name: string; display: string };
	session_id: string;
	turn_id: string;
	title?: string;
	question?: string;
	call?: { name: string; arguments: string };
	context?: InboxContext;
	options: DecisionKind[];
	created_at: string;
	expires_at: string;
}

export interface InboxAnswer {
	decision: DecisionKind;
	value?: string;
}

/** The message and code of a failed inbox call, out of the gateway's envelope. */
export function errorMessage(err: unknown): { code?: string; message: string } {
	if (!(err instanceof ApiError)) return { message: err instanceof Error ? err.message : String(err) };
	const at = err.message.indexOf(' — ');
	try {
		const envelope = JSON.parse(at >= 0 ? err.message.slice(at + 3) : '')?.error;
		if (envelope && typeof envelope.message === 'string') return { code: envelope.code ?? err.code, message: envelope.message };
	} catch {
		// not an envelope
	}
	return { code: err.code, message: err.message };
}

const post = (body: unknown): RequestInit => ({
	method: 'POST',
	headers: { 'content-type': 'application/json' },
	body: JSON.stringify(body)
});

export const INBOX_EVENTS = '/api/v0/agents/inbox/events';

export const inboxApi = {
	list: () => request<{ items: InboxItem[]; count: number }>('/api/v0/agents/inbox').then((r) => r.items),
	answer: (id: string, answer: InboxAnswer) =>
		request<{ turn_id: string }>(`/api/v0/agents/inbox/${encodeURIComponent(id)}/answer`, post(answer))
};

/** The count an `inbox` frame of the events stream carries, or `null` for anything else. */
export function countFromFrame(data: string): number | null {
	try {
		const parsed = JSON.parse(data);
		return parsed && parsed.type === 'inbox' && Number.isInteger(parsed.count) && parsed.count >= 0 ? parsed.count : null;
	} catch {
		return null;
	}
}

/** Fluent key of an item's kind badge. */
export function kindLabel(kind: InboxKind): string {
	return kind === 'approval' ? 'inbox-kind-approval' : 'inbox-kind-handoff';
}

/** What the item's heading names: the agent, or the viewer's own run. */
export function itemHeading(item: InboxItem): { key: string; name: string } {
	if (item.agent) return { key: 'inbox-item-agent', name: item.agent.display || item.agent.name };
	return { key: 'inbox-item-run', name: item.title ?? '' };
}

/**
 * Where the item may link to. Only a manager may open the agent; a
 * responder gets nothing beyond the item. An owner opens their own chat.
 */
export function itemLink(item: InboxItem, base = ''): { href: string; key: string } | null {
	if (item.standing === 'owner') return { href: `${base}/chat/${item.session_id}`, key: 'inbox-open-chat' };
	if (item.standing === 'manager' && item.agent) return { href: `${base}/agents/${item.agent.id}`, key: 'inbox-open-agent' };
	return null;
}

/** An approval's arguments, pretty-printed when they are JSON. */
export function prettyArguments(raw: string): string {
	try {
		return JSON.stringify(JSON.parse(raw), null, 2);
	} catch {
		return raw;
	}
}

/** A slot's value for display: strings as they are, everything else as JSON. */
export function slotText(slot: InboxSlot): string | null {
	if (slot.value === undefined) return null;
	return typeof slot.value === 'string' ? slot.value : JSON.stringify(slot.value);
}

/** Minutes until the deadline, never negative; `null` for an unreadable date. */
export function minutesLeft(expiresAt: string, now = Date.now()): number | null {
	const at = Date.parse(expiresAt);
	if (Number.isNaN(at)) return null;
	return Math.max(0, Math.ceil((at - now) / 60000));
}

/** The answer a decision button sends; `null` when a `value` answer has no text. */
export function answerFor(decision: DecisionKind, text: string): InboxAnswer | null {
	if (decision !== 'value') return { decision };
	const value = text.trim();
	return value ? { decision, value } : null;
}

/** The item a notification link (`?item=…`) names, if it is still listed. */
export function focused(items: InboxItem[], search: URLSearchParams): string | null {
	const id = search.get('item');
	return id && items.some((i) => i.id === id) ? id : null;
}
