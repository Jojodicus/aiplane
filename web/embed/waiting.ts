/**
 * A conversation that waits for a member of staff (#96): an approval or a
 * handoff to a person, whose `suspended` frame offers the visitor no decision
 * (`options` empty). Nothing pushes the answer to the widget, so while it
 * waits the widget re-attaches to the event stream every [`WAIT_POLL_MS`];
 * once staff answered, the resumed turn's answer arrives like any other.
 *
 * A `suspended` frame that does offer the visitor something (a secure input)
 * is not a wait for staff and is left to its own view.
 */
import type { Frame } from './frames.ts';

export const WAIT_POLL_MS = 10_000;

export interface Waiting {
	kind: string;
	turnId: string;
}

function asWaiting(turnId: unknown, suspension: unknown): Waiting | null {
	if (!suspension || typeof suspension !== 'object') return null;
	const s = suspension as Record<string, unknown>;
	const options = Array.isArray(s.options) ? s.options : [];
	if (options.length > 0 || typeof s.kind !== 'string') return null;
	return { kind: s.kind, turnId: String(turnId ?? '') };
}

/** The staff wait a list of turns (a snapshot, `GET /api/v0/embed/session`) ends on. */
export function waitingFromTurns(turns: unknown[]): Waiting | null {
	for (let i = turns.length - 1; i >= 0; i--) {
		const entry = turns[i] as Record<string, unknown> | null;
		const turn = entry?.turn as Record<string, unknown> | undefined;
		if (turn?.status === 'suspended') return asWaiting(turn.id, entry?.suspension);
	}
	return null;
}

/** How `frame` changes what the conversation waits for. */
export function trackWaiting(current: Waiting | null, frame: Frame): Waiting | null {
	switch (frame.event) {
		case 'snapshot':
			return waitingFromTurns(Array.isArray(frame.data.turns) ? frame.data.turns : []);
		case 'suspended': {
			const data = frame.data;
			return asWaiting(data.turn_id, data.suspension ?? data);
		}
		case 'turn_delta':
		case 'turn_finalized':
			return null;
		default:
			return current;
	}
}

/** The Fluent key of the waiting notice. */
export function waitingLabel(waiting: Waiting): string {
	return waiting.kind === 'approval' ? 'embed-waiting-approval' : 'embed-waiting-staff';
}
