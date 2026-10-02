import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseBlocks, parseInline } from './markdown.ts';

test('raw HTML stays literal text', () => {
	const blocks = parseBlocks('<img src=x onerror=alert(1)> <script>x</script>');
	assert.deepEqual(blocks, [
		{
			kind: 'paragraph',
			children: [{ kind: 'text', text: '<img src=x onerror=alert(1)> <script>x</script>' }]
		}
	]);
});

test('only http and https links become links', () => {
	const [ok] = parseInline('[docs](https://example.com/a?b=1)');
	assert.equal(ok.kind, 'link');
	for (const bad of ['javascript:alert', 'data:text/html,x', '//evil.test', 'vbscript:x']) {
		assert.deepEqual(parseInline(`[click](${bad})`), [{ kind: 'text', text: 'click' }]);
	}
});

test('inline code, bold and emphasis nest as expected', () => {
	assert.deepEqual(parseInline('a `b*c*` **d *e* f**'), [
		{ kind: 'text', text: 'a ' },
		{ kind: 'code', text: 'b*c*' },
		{ kind: 'text', text: ' ' },
		{
			kind: 'strong',
			children: [
				{ kind: 'text', text: 'd ' },
				{ kind: 'em', children: [{ kind: 'text', text: 'e' }] },
				{ kind: 'text', text: ' f' }
			]
		}
	]);
});

test('lists, headings, fences and paragraphs split into blocks', () => {
	const kinds = parseBlocks('# Title\n\nIntro\nline\n\n- a\n- b\n\n1. one\n2. two\n\n```js\nlet x = 1;\n\nlet y;\n```\n').map(
		(b) => b.kind
	);
	assert.deepEqual(kinds, ['heading', 'paragraph', 'list', 'list', 'code']);
});

test('list items accumulate and keep their order flag', () => {
	const [list] = parseBlocks('- a\n- b');
	assert.equal(list.kind === 'list' && list.items.length, 2);
	const [ordered] = parseBlocks('1) a\n2) b');
	assert.equal(ordered.kind === 'list' && ordered.ordered, true);
});

test('an unterminated fence keeps what was streamed so far', () => {
	assert.deepEqual(parseBlocks('```\nabc'), [{ kind: 'code', text: 'abc' }]);
});

test('empty and whitespace input yields nothing', () => {
	assert.deepEqual(parseBlocks(' \n\n '), []);
});
