/**
 * Makes a Tailwind v4 + daisyUI v5 stylesheet work inside a shadow root.
 *
 * Both are written for a document: daisyUI hangs its theme variables on
 * `:root`, which never matches inside a shadow tree, and Tailwind declares
 * its `--tw-*` defaults with `@property`, which a shadow tree ignores. So
 * `:root` becomes `:host` (the theme then follows attributes on the host
 * element, e.g. `data-theme`), and each `@property` default becomes a plain
 * custom-property declaration the shadow tree does honour.
 */
export function forShadowRoot(css: string): string {
	const defaults: string[] = [];
	const withoutProperties = css.replace(
		/@property\s+(--[\w-]+)\s*\{([^}]*)\}/g,
		(_rule, name: string, body: string) => {
			const initial = /initial-value\s*:\s*([^;}]*)/.exec(body)?.[1]?.trim();
			if (initial) defaults.push(`${name}:${initial}`);
			return '';
		}
	);
	const hostRules = withoutProperties.replaceAll(':root', ':host');
	const reset = ':host{all:initial}';
	const seeded = defaults.length ? `*,:before,:after,::backdrop{${defaults.join(';')}}` : '';
	return `${reset}${hostRules}${seeded}`;
}

/**
 * Constructed stylesheets are not subject to a page's `style-src` CSP, a
 * `<style>` element would be. Browsers without them get the element. Called
 * again (the agent's colour arrives after the widget is drawn), it replaces
 * what it applied before.
 */
export function applyStyles(root: ShadowRoot, ...sheets: string[]): void {
	const css = sheets.filter(Boolean);
	try {
		root.adoptedStyleSheets = css.map((text) => {
			const sheet = new CSSStyleSheet();
			sheet.replaceSync(text);
			return sheet;
		});
	} catch {
		let style = root.querySelector<HTMLStyleElement>('style[data-croit-aiplane]');
		if (!style) {
			style = document.createElement('style');
			style.setAttribute('data-croit-aiplane', '');
			root.prepend(style);
		}
		style.textContent = css.join('\n');
	}
}
