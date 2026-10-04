// The inbox's live count, shared by the sidebar badge and the inbox page.
//
// One `EventSource` per tab on `GET /api/v0/agents/inbox/events`. Unlike the
// chat stream, this one is meant to stay open: the server never ends it on
// purpose, so the browser's own reconnect is what we want. Every frame bumps
// `version`, which the inbox page watches to refetch its list.
import { browser } from '$app/environment';
import { base } from '$app/paths';
import { frameOf, INBOX_EVENTS } from './inbox.ts';

export const inboxLive = $state({ count: 0, answers: false, version: 0 });

let source: EventSource | null = null;

/** Open the stream (idempotent per tab). */
export function watchInbox(): void {
	if (!browser || source) return;
	source = new EventSource(`${base}${INBOX_EVENTS}`, { withCredentials: true });
	source.addEventListener('inbox', (event) => {
		const frame = frameOf((event as MessageEvent<string>).data);
		if (frame === null) return;
		inboxLive.count = frame.count;
		inboxLive.answers = frame.answers;
		inboxLive.version += 1;
	});
}
