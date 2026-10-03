/**
 * The widget's DOM: a launcher and a chat panel inside one shadow root.
 *
 * Everything the page, the visitor or the model supplies reaches the DOM as
 * `textContent` or an attribute value, never as markup. Classes are daisyUI
 * components plus Tailwind utilities, compiled into `embed.css`.
 */
import { EmbedApi, EmbedError, type AgentView } from './api.ts';
import { MicRecording, Speaker, canRecord } from './audio.ts';
import { applyFrame, emptyConversation, fromTurns, type Conversation, type Message } from './conversation.ts';
import { isSafeHref } from '../shared/url.ts';
import { parseBlocks, type Block, type Inline } from './markdown.ts';
import { secureInputForm, secureRequest } from './secure-input.ts';
import { trackWaiting, waitingFromTurns, waitingLabel, WAIT_POLL_MS, type Waiting } from './waiting.ts';
import { MAX_RECORDING_MS, idleMic, micErrorKey, micStep, nextToSpeak, type MicEvent } from './voice.ts';

export interface WidgetOptions {
	api: EmbedApi;
	t: (key: string) => string;
	title: string | null;
	position: 'left' | 'right';
	reducedMotion: boolean;
	/** The agent's description arrived or changed: its colour is the page's to apply. */
	onAgent?: (agent: AgentView) => void;
}

const MAX_MESSAGE_CHARS = 8000;
const MAX_REATTACH = 3;

type Attrs = Record<string, string>;

function h<K extends keyof HTMLElementTagNameMap>(
	tag: K,
	className: string,
	attrs: Attrs = {},
	...children: Array<Node | string>
): HTMLElementTagNameMap[K] {
	const el = document.createElement(tag);
	el.className = className;
	for (const [name, value] of Object.entries(attrs)) el.setAttribute(name, value);
	el.append(...children);
	return el;
}

function svg(path: string): SVGSVGElement {
	const ns = 'http://www.w3.org/2000/svg';
	const el = document.createElementNS(ns, 'svg');
	el.setAttribute('viewBox', '0 0 24 24');
	el.setAttribute('fill', 'none');
	el.setAttribute('stroke', 'currentColor');
	el.setAttribute('stroke-width', '2');
	el.setAttribute('stroke-linecap', 'round');
	el.setAttribute('stroke-linejoin', 'round');
	el.setAttribute('aria-hidden', 'true');
	el.setAttribute('class', 'size-6');
	const p = document.createElementNS(ns, 'path');
	p.setAttribute('d', path);
	el.append(p);
	return el;
}

const ICON_CHAT = 'M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12Z';
const ICON_CLOSE = 'M6 6l12 12M18 6 6 18';
const ICON_MIC = 'M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3Zm7 9a7 7 0 0 1-14 0M12 19v3';
const ICON_SPEAKER = 'M11 5 6 9H3v6h3l5 4V5Zm4.5 3.5a5 5 0 0 1 0 7M18.5 5.5a9 9 0 0 1 0 13';
const ICON_STOP = 'M7 7h10v10H7z';

function inline(nodes: Inline[]): Node[] {
	return nodes.map((node): Node => {
		switch (node.kind) {
			case 'text':
				return document.createTextNode(node.text);
			case 'code':
				return h('code', 'bg-base-200 rounded px-1 text-[0.9em]', {}, node.text);
			case 'strong':
				return h('strong', 'font-semibold', {}, ...inline(node.children));
			case 'em':
				return h('em', '', {}, ...inline(node.children));
			case 'link': {
				const a = h('a', 'link link-primary', { rel: 'noopener noreferrer nofollow', target: '_blank' }, ...inline(node.children));
				if (isSafeHref(node.href)) a.setAttribute('href', node.href);
				return a;
			}
		}
	});
}

function block(node: Block): Node {
	switch (node.kind) {
		case 'paragraph':
			return h('p', 'whitespace-pre-wrap break-words', {}, ...inline(node.children));
		case 'heading':
			return h('p', 'font-semibold break-words', {}, ...inline(node.children));
		case 'code':
			return h('pre', 'bg-base-200 rounded-field p-2 overflow-x-auto text-sm', {}, h('code', '', {}, node.text));
		case 'list':
			return h(
				node.ordered ? 'ol' : 'ul',
				node.ordered ? 'list-decimal ml-5' : 'list-disc ml-5',
				{},
				...node.items.map((item) => h('li', 'break-words', {}, ...inline(item)))
			);
	}
}

export class Widget {
	private readonly o: WidgetOptions;
	private state: Conversation = emptyConversation();
	private title: string;
	private notice: string | null = null;
	private error: string | null = null;
	private open = false;
	private stream: AbortController | null = null;
	private waiting: Waiting | null = null;
	private rendered = new Map<string, { el: HTMLElement; signature: string }>();
	private voice = { input: false, output: false };
	private micState = idleMic();
	private recording: MicRecording | null = null;
	private recordingLimit: ReturnType<typeof setTimeout> | null = null;
	private speakAloud = false;
	private readonly heard = new Set<string>();
	private readonly speaker = new Speaker();

	private readonly launcher: HTMLButtonElement;
	private readonly panel: HTMLElement;
	private readonly heading: HTMLElement;
	private readonly log: HTMLElement;
	private readonly input: HTMLTextAreaElement;
	private readonly send: HTMLButtonElement;
	private readonly closeButton: HTMLButtonElement;
	private readonly newConversation: HTMLButtonElement;
	private readonly mic: HTMLButtonElement;
	private readonly micStatus: HTMLElement;
	private readonly micStatusText: HTMLElement;
	private readonly micCancel: HTMLButtonElement;
	private readonly speakToggle: HTMLButtonElement;
	private readonly stopSpeaking: HTMLButtonElement;
	readonly element: HTMLElement;

	constructor(options: WidgetOptions) {
		this.o = options;
		const t = options.t;
		this.title = options.title ?? t('embed-default-title');
		const panelId = 'croit-aiplane-panel';
		const side = options.position === 'left' ? 'left-4 items-start' : 'right-4 items-end';

		this.heading = h('h2', 'font-semibold truncate', { id: 'croit-aiplane-title' }, this.title);
		this.newConversation = h('button', 'btn btn-ghost btn-sm text-primary-content', { type: 'button' }, t('embed-new-conversation'));
		this.closeButton = h('button', 'btn btn-ghost btn-sm btn-circle text-primary-content', { type: 'button', 'aria-label': t('embed-launcher-close') }, svg(ICON_CLOSE));
		this.log = h('div', 'flex-1 overflow-y-auto p-4 space-y-3', { role: 'log', 'aria-live': 'polite', 'aria-relevant': 'additions', tabindex: '0', 'aria-labelledby': 'croit-aiplane-title' });
		this.input = h('textarea', 'textarea flex-1 resize-none text-base min-h-10 py-2', { id: 'croit-aiplane-input', rows: '1', maxlength: String(MAX_MESSAGE_CHARS), placeholder: t('embed-input-placeholder'), autocomplete: 'off' });
		this.send = h('button', 'btn btn-primary', { type: 'submit' }, t('embed-send'));
		this.mic = h('button', 'btn btn-ghost btn-square', { type: 'button', hidden: '', 'aria-pressed': 'false', 'aria-label': t('embed-voice-record'), title: t('embed-voice-record') }, svg(ICON_MIC));
		this.micStatusText = h('span', 'flex-1', {});
		this.micCancel = h('button', 'btn btn-ghost btn-xs', { type: 'button' }, t('embed-voice-cancel'));
		this.micStatus = h('div', 'flex items-center gap-2 px-3 pt-2 text-sm', { role: 'status', hidden: '' }, this.micStatusText, this.micCancel);
		const form = h(
			'form',
			'flex items-end gap-2 p-3 border-t border-base-300',
			{},
			h('label', 'sr-only', { for: 'croit-aiplane-input' }, t('embed-input-label')),
			this.mic,
			this.input,
			this.send
		);
		this.speakToggle = h('button', 'btn btn-ghost btn-sm btn-square text-primary-content', { type: 'button', hidden: '', 'aria-pressed': 'false', 'aria-label': t('embed-voice-read-aloud'), title: t('embed-voice-read-aloud') }, svg(ICON_SPEAKER));
		this.stopSpeaking = h('button', 'btn btn-ghost btn-sm btn-square text-primary-content', { type: 'button', hidden: '', 'aria-label': t('embed-voice-stop-speaking'), title: t('embed-voice-stop-speaking') }, svg(ICON_STOP));
		this.panel = h(
			'section',
			'card bg-base-100 text-base-content shadow-xl border border-base-300 flex flex-col w-[22rem] h-[32rem] max-w-[calc(100vw-2rem)] max-h-[calc(100dvh-6.5rem)] max-sm:fixed max-sm:inset-0 max-sm:z-10 max-sm:w-auto max-sm:h-auto max-sm:max-w-none max-sm:max-h-none max-sm:rounded-none',
			{ id: panelId, role: 'dialog', 'aria-labelledby': 'croit-aiplane-title', hidden: '' },
			h('header', 'flex items-center justify-between gap-2 bg-primary text-primary-content px-4 py-2 rounded-t-box max-sm:rounded-none', {}, this.heading, h('div', 'flex items-center gap-1', {}, this.stopSpeaking, this.speakToggle, this.newConversation, this.closeButton)),
			this.log,
			this.micStatus,
			form
		);
		this.launcher = h('button', 'btn btn-primary btn-circle btn-lg shadow-lg', { type: 'button', 'aria-label': t('embed-launcher-open'), 'aria-expanded': 'false', 'aria-controls': panelId }, svg(ICON_CHAT));
		this.element = h('div', `fixed bottom-4 ${side} z-[2147483000] flex flex-col gap-3 font-sans text-base`, {}, this.panel, this.launcher);

		this.launcher.addEventListener('click', () => this.toggle());
		this.closeButton.addEventListener('click', () => this.toggle(false));
		this.newConversation.addEventListener('click', () => this.reset());
		this.panel.addEventListener('keydown', (e) => {
			if (e.key !== 'Escape') return;
			if (this.micState.phase === 'starting' || this.micState.phase === 'recording') this.micEvent({ type: 'cancel' });
			else this.toggle(false);
		});
		this.mic.addEventListener('pointerdown', (e) => {
			if (e.button !== 0) return;
			e.preventDefault();
			this.mic.focus();
			try {
				this.mic.setPointerCapture(e.pointerId);
			} catch {
				// Capture only keeps the release on the button; recording works without it.
			}
			this.micEvent({ type: 'down', at: performance.now() });
		});
		for (const type of ['pointerup', 'pointercancel'] as const) {
			this.mic.addEventListener(type, () => this.micEvent({ type: 'up', at: performance.now() }));
		}
		// Enter and Space click a focused button without any pointer: that toggles.
		this.mic.addEventListener('click', (e) => {
			if (e.detail === 0) this.micEvent({ type: 'toggle' });
		});
		this.micCancel.addEventListener('click', () => this.micEvent({ type: 'cancel' }));
		this.speakToggle.addEventListener('click', () => {
			this.speakAloud = !this.speakAloud;
			if (this.speakAloud) {
				this.speaker.unlock();
				nextToSpeak(this.state.messages, this.heard);
			}
			else this.speaker.stop();
			this.renderSpeaker();
		});
		this.stopSpeaking.addEventListener('click', () => this.speaker.stop());
		this.speaker.onchange = () => this.renderSpeaker();
		form.addEventListener('submit', (e) => {
			e.preventDefault();
			void this.submit();
		});
		this.input.addEventListener('keydown', (e) => {
			if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
				e.preventDefault();
				void this.submit();
			}
		});
		this.input.addEventListener('input', () => this.syncInput());
		this.syncInput();
		this.renderLog();
	}

	async resume(): Promise<void> {
		try {
			const session = await this.o.api.resume();
			if (!session) return;
			this.state = fromTurns(session.turns, session.live_turn_id);
			this.waiting = waitingFromTurns(session.turns);
			this.applyAgent(session.agent);
			this.renderLog();
			if (this.state.pending || this.waiting) void this.follow();
		} catch {
			// The panel works without the old transcript; sending reports real trouble.
		}
	}

	/** What the gateway says about the agent: its name, colour and which voice directions it offers. */
	applyAgent(agent: AgentView): void {
		this.setTitle(agent.display);
		this.voice = { input: !!agent.voice?.input, output: !!agent.voice?.output };
		if (!this.voice.output && this.speakAloud) {
			this.speakAloud = false;
			this.speaker.stop();
		}
		this.renderMic();
		this.renderSpeaker();
		this.o.onAgent?.(agent);
	}

	private setTitle(display: string): void {
		if (this.o.title) return;
		this.title = display || this.o.t('embed-default-title');
		this.heading.textContent = this.title;
	}

	private toggle(next = !this.open): void {
		this.open = next;
		this.panel.toggleAttribute('hidden', !next);
		this.launcher.setAttribute('aria-expanded', String(next));
		this.launcher.setAttribute('aria-label', this.o.t(next ? 'embed-launcher-close' : 'embed-launcher-open'));
		if (next) {
			this.scrollToEnd();
			this.input.focus();
		} else {
			this.launcher.focus();
		}
	}

	private reset(): void {
		this.stream?.abort();
		this.micEvent({ type: 'cancel' });
		this.speaker.stop();
		this.o.api.forget();
		this.state = emptyConversation();
		this.waiting = null;
		this.notice = null;
		this.error = null;
		this.renderLog();
		this.syncInput();
		this.input.focus();
	}

	private syncInput(): void {
		this.input.style.height = 'auto';
		this.input.style.height = `${Math.min(this.input.scrollHeight, 128)}px`;
		this.send.disabled = this.input.value.trim() === '' || this.state.pending;
	}

	private async submit(): Promise<void> {
		const text = this.input.value.trim();
		if (!text || this.state.pending) return;
		if (text.length > MAX_MESSAGE_CHARS) return this.fail('embed-error-too-long');
		this.input.value = '';
		this.error = null;
		this.notice = null;
		this.state.messages.push({ id: `local-${Date.now()}`, role: 'user', text, status: 'completed', failed: false });
		this.state.pending = true;
		this.syncInput();
		this.renderLog();
		try {
			const { restarted, agent } = await this.o.api.send(text);
			if (agent) this.applyAgent(agent);
			if (restarted) {
				this.state.messages = this.state.messages.slice(-1);
				this.notice = this.o.t('embed-session-restarted');
			}
		} catch (error) {
			this.state.messages.pop();
			this.state.pending = false;
			this.input.value = text;
			return this.fail(errorKey(error));
		}
		void this.follow();
	}

	/** Answer the secure field: the code goes to `/embed/resume`, never into the transcript. */
	private async answerWaiting(decision: 'value' | 'deny', code?: string): Promise<void> {
		const waiting = secureRequest(this.state.waiting);
		if (!waiting) return;
		this.error = null;
		this.state.waiting = null;
		this.state.pending = true;
		this.syncInput();
		this.renderLog();
		try {
			await this.o.api.answer(waiting.request_id, decision, code);
		} catch (error) {
			this.state.waiting = waiting;
			return this.fail(errorKey(error));
		}
		void this.follow();
	}

	private micEvent(event: MicEvent): void {
		const [next, effect] = micStep(this.micState, event);
		this.micState = next;
		this.renderMic();
		if (effect === 'start') void this.startRecording();
		else if (effect === 'send') void this.finishRecording();
		else if (effect === 'abort') void this.dropRecording();
	}

	private async startRecording(): Promise<void> {
		this.error = null;
		this.renderLog();
		if (!canRecord()) {
			this.micEvent({ type: 'failed' });
			return this.showError('embed-voice-unsupported');
		}
		try {
			const recording = await MicRecording.open(this.o.api.recorderUrl);
			if (this.micState.phase !== 'starting') return void (await recording.abort());
			this.recording = recording;
			this.recordingLimit = setTimeout(() => this.micEvent({ type: 'limit' }), MAX_RECORDING_MS);
			this.micEvent({ type: 'started' });
		} catch (error) {
			this.micEvent({ type: 'failed' });
			this.showError(micErrorKey(error));
		}
	}

	/** The transcript goes into the input for the visitor to read and send; nothing is sent for them. */
	private async finishRecording(): Promise<void> {
		const recording = this.takeRecording();
		try {
			if (!recording) return;
			const { text, agent } = await this.o.api.transcribe(await recording.stop());
			if (agent) this.applyAgent(agent);
			if (!text.trim()) return this.showError('embed-voice-empty');
			const before = this.input.value.trimEnd();
			this.input.value = (before ? `${before} ${text.trim()}` : text.trim()).slice(0, MAX_MESSAGE_CHARS);
			this.syncInput();
			this.input.focus();
		} catch (error) {
			this.showError(voiceErrorKey(error));
		} finally {
			this.micEvent({ type: 'done' });
		}
	}

	private async dropRecording(): Promise<void> {
		await this.takeRecording()?.abort();
	}

	private takeRecording(): MicRecording | null {
		if (this.recordingLimit) clearTimeout(this.recordingLimit);
		this.recordingLimit = null;
		const recording = this.recording;
		this.recording = null;
		return recording;
	}

	private async speakTurn(turnId: string): Promise<void> {
		try {
			const audio = await this.o.api.speak(turnId);
			if (audio && this.speakAloud) await this.speaker.play(audio);
		} catch {
			this.showError('embed-voice-speak-failed');
		}
	}

	private renderMic(): void {
		const t = this.o.t;
		const { phase } = this.micState;
		const active = phase === 'starting' || phase === 'recording';
		this.mic.hidden = !this.voice.input;
		this.mic.disabled = phase === 'sending';
		this.mic.className = active ? `btn btn-error btn-square${this.o.reducedMotion ? '' : ' animate-pulse'}` : 'btn btn-ghost btn-square';
		this.mic.setAttribute('aria-pressed', String(active));
		const label = t(active ? 'embed-voice-stop-recording' : 'embed-voice-record');
		this.mic.setAttribute('aria-label', label);
		this.mic.title = label;
		this.mic.replaceChildren(phase === 'sending' && !this.o.reducedMotion ? h('span', 'loading loading-spinner loading-sm', { 'aria-hidden': 'true' }) : svg(ICON_MIC));
		this.micStatus.hidden = phase === 'idle';
		this.micStatusText.textContent = phase === 'sending' ? t('embed-voice-transcribing') : phase === 'idle' ? '' : t('embed-voice-recording');
		this.micCancel.hidden = !active;
	}

	private renderSpeaker(): void {
		this.speakToggle.hidden = !this.voice.output;
		this.speakToggle.setAttribute('aria-pressed', String(this.speakAloud));
		this.speakToggle.classList.toggle('btn-active', this.speakAloud);
		this.stopSpeaking.hidden = !this.speaker.playing;
	}

	/** An error that leaves the conversation as it is. */
	private showError(key: string): void {
		this.error = this.o.t(key);
		this.renderLog();
	}

	private fail(key: string): void {
		this.error = this.o.t(key);
		this.state.pending = false;
		this.syncInput();
		this.renderLog();
	}

	private async follow(): Promise<void> {
		this.stream?.abort();
		const controller = new AbortController();
		this.stream = controller;
		for (let attempt = 0; attempt <= MAX_REATTACH && !controller.signal.aborted; attempt++) {
			try {
				for await (const frame of this.o.api.events(controller.signal)) {
					applyFrame(this.state, frame);
					const spoken = this.speakAloud ? nextToSpeak(this.state.messages, this.heard) : null;
					if (spoken) void this.speakTurn(spoken);
					this.waiting = trackWaiting(this.waiting, frame);
					this.syncInput();
					this.renderLog();
				}
			} catch (error) {
				if (controller.signal.aborted) return;
				if (error instanceof EmbedError && error.sessionLost) {
					this.o.api.forget();
					this.state = emptyConversation();
					this.notice = this.o.t('embed-session-restarted');
					this.syncInput();
					return this.renderLog();
				}
				if (attempt === MAX_REATTACH) return this.fail(errorKey(error));
			}
			if (this.waiting && !controller.signal.aborted) {
				// Staff answer on their own time; look again until they did.
				attempt = -1;
				await new Promise((resolve) => setTimeout(resolve, WAIT_POLL_MS));
				continue;
			}
			if (!this.state.pending) return;
			await new Promise((resolve) => setTimeout(resolve, 1000 * (attempt + 1)));
		}
		if (!controller.signal.aborted && this.state.pending) this.fail('embed-error-network');
	}

	private renderLog(): void {
		const t = this.o.t;
		const items: Array<{ key: string; signature: string; build: () => HTMLElement }> = [];
		for (const m of this.state.messages) {
			items.push({ key: m.id, signature: `${m.role}|${m.failed}|${m.text}`, build: () => this.bubble(m) });
		}
		if (this.notice) {
			const notice = this.notice;
			items.push({ key: 'notice', signature: notice, build: () => h('div', 'alert alert-info alert-soft text-sm', { role: 'status' }, notice) });
		}
		const secure = secureRequest(this.state.waiting);
		if (secure) {
			items.push({
				key: 'secure-input',
				signature: secure.request_id,
				build: () =>
					secureInputForm(secure, t, {
						submit: (code) => void this.answerWaiting('value', code),
						cancel: () => void this.answerWaiting('deny')
					})
			});
		}
		if (this.state.pending) {
			items.push({
				key: 'pending',
				signature: 'pending',
				build: () =>
					h(
						'div',
						'chat chat-start',
						{ role: 'status' },
						h('div', 'chat-bubble flex items-center gap-2 text-sm', {}, ...(this.o.reducedMotion ? [] : [h('span', 'loading loading-dots loading-sm', { 'aria-hidden': 'true' })]), t('embed-working'))
					)
			});
		}
		if (this.waiting && !this.state.pending) {
			const label = t(waitingLabel(this.waiting));
			items.push({
				key: 'waiting',
				signature: label,
				build: () => h('div', 'alert alert-info alert-soft text-sm', { role: 'status' }, label)
			});
		}
		if (this.error) {
			const error = this.error;
			items.push({ key: 'error', signature: error, build: () => h('div', 'alert alert-error alert-soft text-sm', { role: 'alert' }, error) });
		}

		const nearEnd = this.log.scrollHeight - this.log.scrollTop - this.log.clientHeight < 48;
		const keep = new Set(items.map((i) => i.key));
		for (const [key, entry] of this.rendered) {
			if (!keep.has(key)) {
				entry.el.remove();
				this.rendered.delete(key);
			}
		}
		let ref: ChildNode | null = this.log.firstChild;
		for (const item of items) {
			let entry = this.rendered.get(item.key);
			if (!entry || entry.signature !== item.signature) {
				const el = item.build();
				entry?.el.replaceWith(el);
				entry = { el, signature: item.signature };
				this.rendered.set(item.key, entry);
			}
			if (entry.el === ref) ref = ref.nextSibling;
			else this.log.insertBefore(entry.el, ref);
		}
		if (nearEnd) this.scrollToEnd();
	}

	private bubble(m: Message): HTMLElement {
		const t = this.o.t;
		const mine = m.role === 'user';
		const body = mine
			? h('div', 'chat-bubble chat-bubble-primary whitespace-pre-wrap break-words', {}, m.text)
			: h(
					'div',
					m.failed ? 'chat-bubble chat-bubble-error space-y-2' : 'chat-bubble space-y-2',
					{},
					...parseBlocks(m.text).map(block)
				);
		return h(
			'div',
			mine ? 'chat chat-end' : 'chat chat-start',
			{},
			h('div', 'chat-header text-xs opacity-60', {}, t(mine ? 'embed-speaker-you' : 'embed-speaker-agent')),
			body
		);
	}

	private scrollToEnd(): void {
		this.log.scrollTop = this.log.scrollHeight;
	}
}

function voiceErrorKey(error: unknown): string {
	if (error instanceof EmbedError && error.code === 'audio_too_short') return 'embed-voice-too-short';
	if (error instanceof EmbedError && error.code === 'voice_unavailable') return 'embed-voice-failed';
	return errorKey(error);
}

function errorKey(error: unknown): string {
	if (!(error instanceof EmbedError)) return 'embed-error-generic';
	switch (error.code) {
		case 'network':
			return 'embed-error-network';
		case 'origin_not_allowed':
			return 'embed-error-origin';
		case 'embed_key_invalid':
		case 'embed_key_revoked':
		case 'agent_disabled':
		case 'agent_not_published':
		case 'agent_runtime_unavailable':
			return 'embed-error-unavailable';
		case 'turn_in_progress':
			return 'embed-error-busy';
		default:
			return error.status === 413 || error.code === 'payload_too_large' ? 'embed-error-too-long' : 'embed-error-generic';
	}
}
