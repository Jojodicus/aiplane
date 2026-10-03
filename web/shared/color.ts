/**
 * Colour arithmetic for the agent's colour (`profile.color`): read `#rrggbb`
 * and pick the text colour that reads on it (WCAG contrast). Used by the
 * embed widget and the builder's preview.
 */

const LIGHT_TEXT = '#ffffff';
const DARK_TEXT = '#1d1d1b';

export function parseHex(color: string | null | undefined): [number, number, number] | null {
	const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(color?.trim() ?? '');
	return m ? [parseInt(m[1]!, 16), parseInt(m[2]!, 16), parseInt(m[3]!, 16)] : null;
}

/** WCAG relative luminance of an sRGB colour. */
function luminance([r, g, b]: [number, number, number]): number {
	const linear = (c: number) => {
		const s = c / 255;
		return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

export function contrastRatio(a: [number, number, number], b: [number, number, number]): number {
	const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
	return (hi! + 0.05) / (lo! + 0.05);
}

/** White or near-black, whichever reads better on `background`. */
export function readableText(background: [number, number, number]): string {
	return contrastRatio(background, parseHex(LIGHT_TEXT)!) >= contrastRatio(background, parseHex(DARK_TEXT)!)
		? LIGHT_TEXT
		: DARK_TEXT;
}
