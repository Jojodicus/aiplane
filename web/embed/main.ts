/**
 * Script entry: `<script src="https://<gateway>/embed.js" data-agent-key="gwe_…" async>`.
 *
 * Reads its own tag's `data-*` attributes, takes the gateway's origin from the
 * script's `src`, and mounts the widget into a shadow root on `<body>`.
 */
import appCss from './embed.css?inline';
import { EmbedApi, TokenStore } from './api.ts';
import { pickLanguage, translator } from './i18n.ts';
import { applyStyles, forShadowRoot } from './styles.ts';
import { Widget } from './widget.ts';

const HOST_TAG = 'croit-aiplane-embed';

function ownScript(): HTMLScriptElement | null {
	const current = document.currentScript;
	if (current instanceof HTMLScriptElement && current.dataset.agentKey) return current;
	const tagged = document.querySelectorAll<HTMLScriptElement>('script[data-agent-key]');
	return tagged.item(tagged.length - 1);
}

function mount(script: HTMLScriptElement): void {
	const key = script.dataset.agentKey ?? '';
	if (!key.startsWith('gwe_')) {
		console.error('croit AIplane embed: data-agent-key must be an embed key starting with "gwe_".');
		return;
	}
	if (document.querySelector(HOST_TAG)) return;

	const lang = pickLanguage(script.dataset.lang, navigator.languages ?? [navigator.language]);
	const host = document.createElement(HOST_TAG);
	const theme = script.dataset.theme;
	if (theme === 'light' || theme === 'dark') host.setAttribute('data-theme', theme);
	const root = host.attachShadow({ mode: 'open' });
	applyStyles(root, forShadowRoot(appCss));

	const api = new EmbedApi({
		base: new URL(script.src, location.href).origin,
		key,
		lang,
		tokens: new TokenStore(() => window.sessionStorage, `croit-aiplane-embed:${key}`)
	});
	// A website that knows who its visitor is vouches for them with a token it
	// signed: up front in `data-identity-token`, or later (after its own login)
	// through `document.querySelector('croit-aiplane-embed').setIdentityToken(t)`.
	Object.assign(host, { setIdentityToken: (token: string | null) => api.setIdentity(token) });
	const widget = new Widget({
		api,
		t: translator(lang),
		title: script.dataset.title?.trim() || null,
		position: script.dataset.position === 'left' ? 'left' : 'right',
		reducedMotion: matchMedia('(prefers-reduced-motion: reduce)').matches
	});
	root.append(widget.element);
	document.body.append(host);
	const identity = script.dataset.identityToken;
	void (identity ? api.setIdentity(identity) : Promise.resolve()).then(() => widget.resume());
}

const script = ownScript();
if (script) {
	if (document.body) mount(script);
	else document.addEventListener('DOMContentLoaded', () => mount(script), { once: true });
}
