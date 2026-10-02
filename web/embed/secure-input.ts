/**
 * The secure field a verifier asks the visitor to type a code into.
 *
 * A `secure_input` pause (`docs/agents.md` "What #95 built") is answered with
 * `POST /api/v0/embed/resume`, never as a chat message: the code goes to the
 * verifier and nowhere else — not into the transcript, the model or a log.
 * The field is masked, offers the browser's one-time-code autofill, and is
 * emptied the moment it is sent.
 */
import type { Waiting } from './api.ts';

/** The request the field answers, when the conversation waits for the visitor's own input. */
export function secureRequest(waiting: Waiting | null): Waiting | null {
	if (!waiting || waiting.kind !== 'secure_input' || !waiting.options.includes('value')) return null;
	return waiting;
}

/** What to send for `typed`: the trimmed code, or `null` when there is nothing to send. */
export function codeToSend(typed: string): string | null {
	const code = typed.trim();
	return code === '' ? null : code;
}

export interface SecureInputHandlers {
	submit: (code: string) => void;
	cancel: (() => void) | null;
}

/**
 * The form for `request`: the gateway's (already translated) message, a
 * masked `one-time-code` field, a confirm and — when the request may be
 * declined — a cancel button. daisyUI classes, text only via `textContent`.
 */
export function secureInputForm(request: Waiting, t: (key: string) => string, on: SecureInputHandlers): HTMLElement {
	const form = document.createElement('form');
	form.className = 'card bg-base-200 border border-base-300 card-body p-3 gap-2';
	form.setAttribute('aria-labelledby', 'croit-aiplane-code-label');

	if (request.message) {
		const message = document.createElement('p');
		message.className = 'text-sm';
		message.textContent = request.message;
		form.append(message);
	}

	const label = document.createElement('label');
	label.className = 'text-sm font-semibold';
	label.id = 'croit-aiplane-code-label';
	label.htmlFor = 'croit-aiplane-code';
	label.textContent = t('embed-code-label');

	const input = document.createElement('input');
	input.className = 'input w-full font-mono tracking-widest';
	input.id = 'croit-aiplane-code';
	input.type = 'password';
	input.name = 'one-time-code';
	input.autocomplete = 'one-time-code';
	input.spellcheck = false;
	input.setAttribute('autocapitalize', 'off');
	input.maxLength = 64;
	input.required = true;

	const hint = document.createElement('p');
	hint.className = 'text-xs text-base-content/60';
	hint.textContent = t('embed-code-hint');

	const confirm = document.createElement('button');
	confirm.className = 'btn btn-primary btn-sm';
	confirm.type = 'submit';
	confirm.textContent = t('embed-code-submit');

	const actions = document.createElement('div');
	actions.className = 'flex justify-end gap-2';
	if (on.cancel && request.options.includes('deny')) {
		const cancel = document.createElement('button');
		cancel.className = 'btn btn-ghost btn-sm';
		cancel.type = 'button';
		cancel.textContent = t('embed-code-cancel');
		const decline = on.cancel;
		cancel.addEventListener('click', () => decline());
		actions.append(cancel);
	}
	actions.append(confirm);

	form.addEventListener('submit', (e) => {
		e.preventDefault();
		const code = codeToSend(input.value);
		input.value = '';
		if (code !== null) on.submit(code);
	});

	form.append(label, input, hint, actions);
	queueMicrotask(() => input.focus());
	return form;
}
