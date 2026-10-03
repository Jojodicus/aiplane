// The two daisyUI themes in app.css are one design in two lightnesses. These
// checks pin the parts of that contract a reviewer cannot see in a diff:
// both themes define the same tokens (a token only one theme sets silently
// falls back to daisyUI's default in the other), the text/fill pairs stay
// readable, and the app-wide field states (focus ring, error) are wired.

import test from 'node:test';
import assert from 'node:assert';
import { readFileSync } from 'node:fs';

const css = readFileSync(new URL('../app.css', import.meta.url).pathname, 'utf8');

function themes(): Map<string, Map<string, string>> {
	const out = new Map<string, Map<string, string>>();
	for (const block of css.matchAll(/@plugin "daisyui\/theme" \{([^}]*)\}/g)) {
		const body = block[1];
		const name = /name:\s*"([^"]+)"/.exec(body)?.[1];
		assert.ok(name, 'a theme block has no name');
		const tokens = new Map<string, string>();
		for (const m of body.matchAll(/(--[\w-]+):\s*([^;]+);/g)) tokens.set(m[1], m[2].trim());
		out.set(name, tokens);
	}
	return out;
}

function luminance(hex: string): number {
	const m = /^#([0-9a-f]{6})$/i.exec(hex);
	assert.ok(m, `${hex} is not a #rrggbb colour — the contrast check needs hex tokens`);
	const [r, g, b] = [0, 2, 4].map((i) => {
		const v = parseInt(m[1].slice(i, i + 2), 16) / 255;
		return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
	});
	return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
	const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
	return (hi + 0.05) / (lo + 0.05);
}

test('app.css defines a light and a dark theme with the same tokens', () => {
	const all = themes();
	assert.deepEqual([...all.keys()].sort(), ['dark', 'light']);
	const [light, dark] = [all.get('light')!, all.get('dark')!];
	assert.deepEqual([...light.keys()].sort(), [...dark.keys()].sort());
	for (const key of ['--radius-field', '--radius-box', '--radius-selector', '--size-field', '--border']) {
		assert.equal(light.get(key), dark.get(key), `${key} differs between themes — the shapes must match`);
	}
});

test('every text-on-fill pair in both themes reaches WCAG AA (4.5:1)', () => {
	const pairs = [
		['--color-base-content', '--color-base-100'],
		['--color-base-content', '--color-base-200'],
		['--color-primary-content', '--color-primary'],
		['--color-neutral-content', '--color-neutral'],
		['--color-success-content', '--color-success'],
		['--color-warning-content', '--color-warning'],
		['--color-error-content', '--color-error'],
		['--color-info-content', '--color-info']
	];
	for (const [name, tokens] of themes()) {
		for (const [fg, bg] of pairs) {
			const ratio = contrast(tokens.get(fg)!, tokens.get(bg)!);
			assert.ok(ratio >= 4.5, `${name}: ${fg} on ${bg} is ${ratio.toFixed(2)}:1`);
		}
	}
});

test('a plain button keeps a visible edge on a card', () => {
	assert.match(css, /:where\(\.btn:not\([^)]*\)\)\s*\{\s*border-color:\s*var\(--color-base-300\)/);
});

test('focused fields ring in the primary colour, invalid ones in error', () => {
	assert.match(css, /:where\(\.input, \.select, \.textarea\):is\(:focus, :focus-within\)\s*\{\s*--input-color:\s*var\(--color-primary\)/, 'no primary focus ring on fields');
	assert.match(css, /\[aria-invalid="true"\][\s\S]*?--input-color:\s*var\(--color-error\)/, 'aria-invalid fields are not marked');
	assert.ok(
		css.indexOf('.input[aria-invalid="true"]') > css.indexOf(':where(.input, .select, .textarea)'),
		'the error rule must follow the focus rule, or a focused invalid field rings in primary'
	);
});
