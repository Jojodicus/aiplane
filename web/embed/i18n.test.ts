import assert from 'node:assert/strict';
import { test } from 'node:test';
import { LANGUAGES, pickLanguage, translator } from './i18n.ts';

test('the six product languages are all present', () => {
	assert.deepEqual([...LANGUAGES].sort(), ['de', 'en', 'es', 'fr', 'ru', 'zh']);
});

test('data-lang beats the browser language, and region tags reduce to the language', () => {
	assert.equal(pickLanguage('fr', ['de-DE']), 'fr');
	assert.equal(pickLanguage(null, ['pt-BR', 'de-AT', 'en']), 'de');
	assert.equal(pickLanguage('ZH_cn', []), 'zh');
});

test('an unsupported language falls back to English', () => {
	assert.equal(pickLanguage('xx', ['ja', 'pt']), 'en');
	assert.equal(pickLanguage(undefined, []), 'en');
});

test('every language defines every key English does', () => {
	const en = translator('en');
	for (const lang of LANGUAGES) {
		const t = translator(lang);
		for (const key of ['embed-send', 'embed-working', 'embed-error-network', 'embed-launcher-open']) {
			assert.notEqual(t(key), key, `${lang} lacks ${key}`);
		}
		if (lang !== 'en') assert.notEqual(t('embed-send'), en('embed-send'));
	}
});

test('an unknown key renders as itself rather than blank', () => {
	assert.equal(translator('de')('embed-nope'), 'embed-nope');
});
