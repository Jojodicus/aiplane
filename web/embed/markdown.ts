/**
 * A deliberately small markdown subset, parsed to a tree and never to HTML.
 *
 * Answers are untrusted text. The SPA runs `marked` + DOMPurify and injects the
 * result; the widget instead builds DOM nodes from this tree with
 * `createElement` and `textContent`, so there is no HTML sink to sanitize and
 * the bundle does not carry a markdown engine. Supported: paragraphs, fenced
 * code, bullet and numbered lists, headings (shown as bold paragraphs),
 * `code`, **bold**, *emphasis* and http(s) links.
 */

export type Inline =
	| { kind: 'text'; text: string }
	| { kind: 'code'; text: string }
	| { kind: 'strong'; children: Inline[] }
	| { kind: 'em'; children: Inline[] }
	| { kind: 'link'; href: string; children: Inline[] };

export type Block =
	| { kind: 'paragraph'; children: Inline[] }
	| { kind: 'heading'; children: Inline[] }
	| { kind: 'code'; text: string }
	| { kind: 'list'; ordered: boolean; items: Inline[][] };

const BULLET = /^\s*[-*+]\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;
const HEADING = /^#{1,6}\s+(.*)$/;
const FENCE = /^\s*```/;

export function parseBlocks(source: string): Block[] {
	const lines = source.replaceAll('\r\n', '\n').split('\n');
	const blocks: Block[] = [];
	let paragraph: string[] = [];
	const flush = () => {
		if (paragraph.length) blocks.push({ kind: 'paragraph', children: parseInline(paragraph.join('\n')) });
		paragraph = [];
	};
	for (let i = 0; i < lines.length; i++) {
		const line = lines[i];
		if (FENCE.test(line)) {
			flush();
			const code: string[] = [];
			for (i++; i < lines.length && !FENCE.test(lines[i]); i++) code.push(lines[i]);
			blocks.push({ kind: 'code', text: code.join('\n') });
			continue;
		}
		const heading = HEADING.exec(line);
		if (heading) {
			flush();
			blocks.push({ kind: 'heading', children: parseInline(heading[1]) });
			continue;
		}
		const item = BULLET.exec(line) ?? NUMBERED.exec(line);
		if (item) {
			flush();
			const ordered = NUMBERED.test(line) && !BULLET.test(line);
			const last = blocks.at(-1);
			if (last?.kind === 'list' && last.ordered === ordered) last.items.push(parseInline(item[1]));
			else blocks.push({ kind: 'list', ordered, items: [parseInline(item[1])] });
			continue;
		}
		if (line.trim() === '') flush();
		else paragraph.push(line);
	}
	flush();
	return blocks;
}

const SAFE_HREF = /^https?:\/\/[^\s<>"']+$/i;

export function isSafeHref(href: string): boolean {
	return SAFE_HREF.test(href);
}

export function parseInline(source: string): Inline[] {
	const out: Inline[] = [];
	let text = '';
	const pushText = () => {
		if (text) out.push({ kind: 'text', text });
		text = '';
	};
	let i = 0;
	while (i < source.length) {
		const rest = source.slice(i);
		let m: RegExpExecArray | null;
		if ((m = /^`([^`\n]+)`/.exec(rest))) {
			pushText();
			out.push({ kind: 'code', text: m[1] });
		} else if ((m = /^\*\*([^*\n](?:[^\n]*?[^*\n])?)\*\*/.exec(rest))) {
			pushText();
			out.push({ kind: 'strong', children: parseInline(m[1]) });
		} else if ((m = /^\*([^*\s](?:[^*\n]*?[^*\s])?)\*/.exec(rest))) {
			pushText();
			out.push({ kind: 'em', children: parseInline(m[1]) });
		} else if ((m = /^\[([^\]\n]+)\]\(([^)\s]+)\)/.exec(rest))) {
			if (isSafeHref(m[2])) {
				pushText();
				out.push({ kind: 'link', href: m[2], children: parseInline(m[1]) });
			} else {
				text += m[1];
			}
		} else {
			text += source[i];
			i++;
			continue;
		}
		i += m[0].length;
	}
	pushText();
	return out;
}
