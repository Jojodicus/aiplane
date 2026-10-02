/**
 * The widget's DOM: a launcher and a chat panel inside one shadow root.
 *
 * Everything the page, the visitor or the model supplies reaches the DOM as
 * `textContent` or an attribute value, never as markup. Classes are daisyUI
 * components plus Tailwind utilities, compiled into `embed.css`.
 */
import { EmbedApi, EmbedError } from './api.ts';
import { applyFrame, emptyConversation, fromTurns, type Conversation, type Message } from './conversation.ts';
import { isSafeHref, parseBlocks, type Block, type Inline } from './markdown.ts';

export interface WidgetOptions {
	api: EmbedApi;
	t: (key: string) => string;
	title: string | null;
	position: 'left' | 'right';
	reducedMotion: boolean;
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
	private rendered = new Map<string, { el: HTMLElement; signature: string }>();

	private readonly launcher: HTMLButtonElement;
	private readonly panel: HTMLElement;
	private readonly heading: HTMLElement;
	private readonly log: HTMLElement;
	private readonly input: HTMLTextAreaElement;
	private readonly send: HTMLButtonElement;
	private readonly closeButton: HTMLButtonElement;
	private readonly newConversation: HTMLButtonElement;
	readonly element: HTMLElement;

	constructor(options: WidgetOptions) {
		this.o = options;
		const t = options.t;
		this.title = options.title ?? t('embed-default-title');
		const panelId = 'croit-aiplane-panel';
		const side = options.position === 'left' ? 'left-4 items-start' : 'right-4 items-end';

		this.heading = h('h2', 'font-semibold truncate', { id: 'croit-aiplane-title' }, this.title);
		this.newConversation = h('button', 'btn btn-ghost btn-sm', { type: 'button' }, t('embed-new-conversation'));
		this.closeButton = h('button', 'btn btn-ghost btn-sm btn-circle', { type: 'button', 'aria-label': t('embed-launcher-close') }, svg(ICON_CLOSE));
		this.log = h('div', 'flex-1 overflow-y-auto p-4 space-y-3', { role: 'log', 'aria-live': 'polite', 'aria-relevant': 'additions', tabindex: '0', 'aria-labelledby': 'croit-aiplane-title' });
		this.input = h('textarea', 'textarea flex-1 resize-none text-base min-h-10 py-2', { id: 'croit-aiplane-input', rows: '1', maxlength: String(MAX_MESSAGE_CHARS), placeholder: t('embed-input-placeholder'), autocomplete: 'off' });
		this.send = h('button', 'btn btn-primary', { type: 'submit' }, t('embed-send'));
		const form = h(
			'form',
			'flex items-end gap-2 p-3 border-t border-base-300',
			{},
			h('label', 'sr-only', { for: 'croit-aiplane-input' }, t('embed-input-label')),
			this.input,
			this.send
		);
		this.panel = h(
			'section',
			'card bg-base-100 text-base-content shadow-xl border border-base-300 flex flex-col w-[22rem] h-[32rem] max-w-[calc(100vw-2rem)] max-h-[calc(100dvh-6.5rem)] max-sm:fixed max-sm:inset-0 max-sm:z-10 max-sm:w-auto max-sm:h-auto max-sm:max-w-none max-sm:max-h-none max-sm:rounded-none',
			{ id: panelId, role: 'dialog', 'aria-labelledby': 'croit-aiplane-title', hidden: '' },
			h('header', 'flex items-center justify-between gap-2 bg-primary text-primary-content px-4 py-2 rounded-t-box max-sm:rounded-none', {}, this.heading, h('div', 'flex items-center gap-1', {}, this.newConversation, this.closeButton)),
			this.log,
			form
		);
		this.launcher = h('button', 'btn btn-primary btn-circle btn-lg shadow-lg', { type: 'button', 'aria-label': t('embed-launcher-open'), 'aria-expanded': 'false', 'aria-controls': panelId }, svg(ICON_CHAT));
		this.element = h('div', `fixed bottom-4 ${side} z-[2147483000] flex flex-col gap-3 font-sans text-base`, {}, this.panel, this.launcher);

		this.launcher.addEventListener('click', () => this.toggle());
		this.closeButton.addEventListener('click', () => this.toggle(false));
		this.newConversation.addEventListener('click', () => this.reset());
		this.panel.addEventListener('keydown', (e) => {
			if (e.key === 'Escape') this.toggle(false);
		});
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
			this.setTitle(session.agent.display);
			this.renderLog();
			if (this.state.pending) void this.follow();
		} catch {
			// The panel works without the old transcript; sending reports real trouble.
		}
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
		this.o.api.forget();
		this.state = emptyConversation();
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
			const { restarted } = await this.o.api.send(text);
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
