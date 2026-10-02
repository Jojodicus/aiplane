/**
 * Strings of the widget, from the same Fluent catalogs as the rest of the
 * product (`embed-*` keys, converted into `locales.generated.ts`).
 */
import { catalogs } from './locales.generated.ts';

export const LANGUAGES = Object.keys(catalogs);
const FALLBACK = 'en';

/** `data-lang` wins; otherwise the browser's language, by primary subtag; otherwise English. */
export function pickLanguage(override: string | null | undefined, navigatorLanguages: readonly string[]): string {
	for (const candidate of [override, ...navigatorLanguages]) {
		const primary = candidate?.trim().toLowerCase().split(/[-_]/)[0];
		if (primary && primary in catalogs) return primary;
	}
	return FALLBACK;
}

export function translator(lang: string): (key: string) => string {
	const catalog = catalogs[lang] ?? catalogs[FALLBACK];
	return (key) => catalog[key] ?? catalogs[FALLBACK][key] ?? key;
}
