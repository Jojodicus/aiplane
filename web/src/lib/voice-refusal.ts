import { refusalSentence, type ApiError } from './api.ts';

/** The gateway's own codes for a caller over a usage limit: a browser session's and a bearer's. */
const LIMIT_CODES = ['rate_limited', 'rate_limit_exceeded'];

/**
 * Whether the gateway refused because the user is over a usage limit. Decided
 * on the gateway's error code, not on the status: a `429` an upstream or a
 * proxy sent is no statement about the user's limits.
 */
export function isLimitRefusal(err: ApiError): boolean {
	return LIMIT_CODES.includes(err.code ?? '');
}

/**
 * What a refused chat voice call (`/api/v0/transcriptions`, `/api/v0/speech`)
 * tells the user: a limit refusal from the catalog, since the server's
 * sentence is English; anything else as {@link refusalSentence} words it.
 */
export function voiceRefusalMessage(
	err: ApiError,
	tr: (key: string, args?: Record<string, string | number>) => string
): string {
	return isLimitRefusal(err) ? tr('voice-limit-reached') : refusalSentence(err, tr);
}
