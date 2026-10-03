/**
 * The microphone button's behaviour, free of the DOM and of audio: one press
 * held down records until release (press-to-talk); a short click starts a
 * recording that the next click sends (click-to-toggle). Enter or Space on
 * the focused button toggles. Cancel throws the recording away. The view
 * feeds events in and performs the effect that comes back.
 */

/** A press held at least this long is press-to-talk; a shorter one toggles. */
export const HOLD_MS = 350;
/** Recording stops and is sent on its own at the gateway's limit. */
export const MAX_RECORDING_MS = 60_000;

export type MicPhase = 'idle' | 'starting' | 'recording' | 'sending';

export interface MicState {
	phase: MicPhase;
	/** The pointer that started the recording is still down. */
	held: boolean;
	pressedAt: number;
}

export type MicEvent =
	| { type: 'down'; at: number }
	| { type: 'up'; at: number }
	| { type: 'toggle' }
	| { type: 'started' }
	| { type: 'failed' }
	| { type: 'cancel' }
	| { type: 'limit' }
	| { type: 'done' };

/** `start`: open the microphone; `send`: stop and transcribe; `abort`: stop and discard. */
export type MicEffect = 'start' | 'send' | 'abort' | null;

export const idleMic = (): MicState => ({ phase: 'idle', held: false, pressedAt: 0 });

export function micStep(state: MicState, event: MicEvent): [MicState, MicEffect] {
	const { phase } = state;
	switch (event.type) {
		case 'down':
			if (phase === 'idle') return [{ phase: 'starting', held: true, pressedAt: event.at }, 'start'];
			if (phase === 'recording' && !state.held) return [{ ...state, phase: 'sending' }, 'send'];
			return [state, null];
		case 'up': {
			if (!state.held) return [state, null];
			const pressToTalk = event.at - state.pressedAt >= HOLD_MS;
			if (phase === 'recording' && pressToTalk) return [{ ...state, phase: 'sending', held: false }, 'send'];
			if (phase === 'starting' || phase === 'recording') return [{ ...state, held: false }, null];
			return [state, null];
		}
		case 'toggle':
			if (phase === 'idle') return [{ phase: 'starting', held: false, pressedAt: 0 }, 'start'];
			if (phase === 'recording') return [{ ...state, phase: 'sending', held: false }, 'send'];
			return [state, null];
		case 'started':
			return phase === 'starting' ? [{ ...state, phase: 'recording' }, null] : [state, null];
		case 'limit':
			return phase === 'recording' ? [{ ...state, phase: 'sending', held: false }, 'send'] : [state, null];
		case 'cancel':
			return phase === 'starting' || phase === 'recording' ? [idleMic(), 'abort'] : [state, null];
		case 'failed':
		case 'done':
			return [idleMic(), null];
	}
}

/** Why the microphone could not be used, as a catalog key. */
export function micErrorKey(error: unknown): string {
	const name = error instanceof Error || error instanceof DOMException ? error.name : '';
	if (name === 'NotAllowedError' || name === 'PermissionDeniedError' || name === 'SecurityError') return 'embed-voice-denied';
	if (name === 'NotFoundError' || name === 'DevicesNotFoundError') return 'embed-voice-no-mic';
	return 'embed-voice-failed';
}

/**
 * The newest finished answer not yet heard, if any, marking every finished
 * answer heard. An answer can arrive in a `turn_finalized` frame or already
 * whole in a `snapshot` (when it finished before the stream attached), so
 * this reads the transcript, not the frame. Mark the transcript heard when
 * the speaker is switched on, so it reads what comes next, not the history.
 */
export function nextToSpeak(
	messages: ReadonlyArray<{ id: string; role: string; status: string; failed: boolean }>,
	heard: Set<string>
): string | null {
	let next: string | null = null;
	for (const m of messages) {
		if (m.role !== 'assistant' || m.status !== 'completed' || m.failed || heard.has(m.id)) continue;
		heard.add(m.id);
		next = m.id;
	}
	return next;
}
