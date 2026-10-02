/**
 * The widget's whole contact with the gateway: `POST /api/v0/embed/sessions`
 * (embed key + the browser's Origin -> `gwv_` visitor token), then the visitor
 * token as `Authorization: Bearer` on every later call. Never cookies, and the
 * only secrets the widget holds are the public embed key and that token.
 */
import { readFrames, type Frame } from './frames.ts';

export interface StorageLike {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
	removeItem(key: string): void;
}

/**
 * The visitor token, in `sessionStorage` so it survives a reload and dies with
 * the tab. Storage can throw or be absent (blocked site data, sandboxed
 * frames), so every access is guarded and the token then lives in memory only.
 */
export class TokenStore {
	private memory: string | null = null;

	private readonly storage: () => StorageLike | null;
	private readonly slot: string;

	constructor(storage: () => StorageLike | null, slot: string) {
		this.storage = storage;
		this.slot = slot;
	}

	load(): string | null {
		try {
			return this.storage()?.getItem(this.slot) ?? this.memory;
		} catch {
			return this.memory;
		}
	}

	save(token: string): void {
		this.memory = token;
		try {
			this.storage()?.setItem(this.slot, token);
		} catch {
			// Memory copy above is the fallback.
		}
	}

	clear(): void {
		this.memory = null;
		try {
			this.storage()?.removeItem(this.slot);
		} catch {
			// Nothing stored that we could not already forget.
		}
	}
}

/** A refusal from the gateway (`{error: {code, message}}`) or a failed connection (`status` 0). */
export class EmbedError extends Error {
	readonly code: string;
	readonly status: number;

	constructor(code: string, status: number, message: string) {
		super(message);
		this.code = code;
		this.status = status;
	}

	get sessionLost(): boolean {
		return this.code === 'visitor_session_expired' || this.code === 'visitor_session_invalid';
	}
}

export interface TurnView {
	id: string;
	role: 'user' | 'assistant';
	user_content: string | null;
	content: string | null;
	status: 'in_progress' | 'suspended' | 'completed' | 'cancelled' | 'errored';
	error_message: string | null;
}

/** What a paused conversation waits for (`SuspensionView::for_participant`): never the tool. */
export interface Waiting {
	request_id: string;
	kind: 'secure_input' | 'approval' | 'human_answer';
	message?: string | null;
	/** The decisions the visitor may make; empty when staff answer. */
	options: Array<'value' | 'deny' | 'allow_once'>;
	expires_at: string;
}

/** The wire shape of a transcript entry: the turn, wrapped (tool calls and steers are always empty for visitors). */
export interface TurnWithTools {
	turn: TurnView;
	suspension?: Waiting | null;
}

export interface SessionView {
	agent: { display: string };
	live_turn_id: string | null;
	turns: TurnWithTools[];
}

export interface EmbedApiOptions {
	base: string;
	key: string;
	lang: string;
	tokens: TokenStore;
	fetch?: typeof fetch;
}

export class EmbedApi {
	private readonly http: typeof fetch;
	private readonly options: EmbedApiOptions;
	private identity: string | null = null;
	private identitySent = false;

	constructor(options: EmbedApiOptions) {
		this.options = options;
		this.http = options.fetch ?? ((input, init) => fetch(input, init));
	}

	hasToken(): boolean {
		return this.options.tokens.load() !== null;
	}

	forget(): void {
		this.options.tokens.clear();
	}

	async start(): Promise<{ agent: { display: string } }> {
		const response = await this.call('/api/v0/embed/sessions', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ key: this.options.key })
		});
		const body = (await response.json()) as { token: string; agent: { display: string } };
		this.options.tokens.save(body.token);
		this.identitySent = false;
		await this.identify();
		return body;
	}

	/** The stored conversation, or `null` when there is none or it timed out. */
	async resume(): Promise<SessionView | null> {
		if (!this.hasToken()) return null;
		try {
			const response = await this.call('/api/v0/embed/session', { headers: this.auth() });
			return (await response.json()) as SessionView;
		} catch (error) {
			if (error instanceof EmbedError && error.sessionLost) {
				this.forget();
				return null;
			}
			throw error;
		}
	}

	/**
	 * Sends one message, starting a session first when there is none. When the
	 * stored one has timed out it starts a fresh one and sends there; `restarted`
	 * tells the caller the earlier conversation is gone.
	 */
	async send(text: string): Promise<{ restarted: boolean }> {
		let restarted = false;
		if (!this.hasToken()) await this.start();
		for (let attempt = 0; ; attempt++) {
			try {
				await this.call('/api/v0/embed/messages', {
					method: 'POST',
					headers: { ...this.auth(), 'content-type': 'application/json' },
					body: JSON.stringify({ text })
				});
				return { restarted };
			} catch (error) {
				if (attempt > 0 || !(error instanceof EmbedError) || !error.sessionLost) throw error;
				this.forget();
				await this.start();
				restarted = true;
			}
		}
	}

	/**
	 * Answers what the conversation waits for: a code typed into the secure
	 * field (`value`), or `deny` to give up. The rest arrives on the event stream.
	 */
	async answer(requestId: string, decision: 'value' | 'deny', value?: string): Promise<void> {
		const body: Record<string, string> = { request_id: requestId, decision };
		if (decision === 'value' && value !== undefined) body.value = value;
		await this.call('/api/v0/embed/resume', {
			method: 'POST',
			headers: { ...this.auth(), 'content-type': 'application/json' },
			body: JSON.stringify(body)
		});
	}

	/**
	 * The host page's signed identity token for this visitor, sent once per
	 * conversation: now if one is open, otherwise right after the next one starts.
	 */
	async setIdentity(token: string | null): Promise<void> {
		this.identity = token?.trim() || null;
		this.identitySent = false;
		if (this.hasToken()) await this.identify();
	}

	private async identify(): Promise<void> {
		if (!this.identity || this.identitySent) return;
		this.identitySent = true;
		try {
			await this.call('/api/v0/embed/identity', {
				method: 'POST',
				headers: { ...this.auth(), 'content-type': 'application/json' },
				body: JSON.stringify({ token: this.identity })
			});
		} catch (error) {
			// The conversation works without it; the site's developer needs to know why.
			const reason = error instanceof EmbedError ? `${error.code}: ${error.message}` : String(error);
			console.warn(`croit AIplane embed: the identity token was refused (${reason}).`);
		}
	}

	/** The frames of the live conversation until the gateway ends the stream. */
	async *events(signal: AbortSignal): AsyncGenerator<Frame> {
		const response = await this.call('/api/v0/embed/events', {
			headers: { ...this.auth(), accept: 'text/event-stream' },
			signal
		});
		if (!response.body) return;
		yield* readFrames(response.body);
	}

	private auth(): Record<string, string> {
		const token = this.options.tokens.load();
		return token ? { authorization: `Bearer ${token}` } : {};
	}

	private async call(path: string, init: RequestInit): Promise<Response> {
		let response: Response;
		try {
			response = await this.http(this.options.base + path, {
				...init,
				credentials: 'omit',
				headers: { 'accept-language': this.options.lang, ...(init.headers as Record<string, string>) }
			});
		} catch (error) {
			if (error instanceof DOMException && error.name === 'AbortError') throw error;
			throw new EmbedError('network', 0, 'the gateway could not be reached');
		}
		if (response.ok) return response;
		let code = 'unknown';
		let message = `the gateway answered ${response.status}`;
		try {
			const body = (await response.json()) as { error?: { code?: string; message?: string } };
			code = body.error?.code ?? code;
			message = body.error?.message ?? message;
		} catch {
			// Not the JSON error envelope; the status stays the only detail.
		}
		throw new EmbedError(code, response.status, message);
	}
}
