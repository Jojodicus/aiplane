/**
 * The agent's colour (`profile.color`, `#rrggbb`) as the widget's primary
 * colour, with the text on it chosen for contrast. It is a `:host` rule in
 * the shadow root, so it beats the built-in theme but a website's own
 * `croit-aiplane-embed { --color-primary: … }` still beats it.
 */

import { parseHex, readableText } from '../shared/color.ts';

/** The rule that paints the widget in `color`; empty when it is not a `#rrggbb` colour. */
export function agentThemeCss(color: string | null | undefined): string {
	const rgb = parseHex(color);
	if (!rgb) return '';
	const hex = color!.trim().toLowerCase();
	// Four `:host`s outrank the theme's `:host:not([data-theme])`-style rules
	// inside the shadow root; rules from the page outrank any of them.
	return `:host:host:host:host{--color-primary:${hex};--color-primary-content:${readableText(rgb)}}`;
}
