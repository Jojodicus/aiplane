/**
 * Which pages are a conversation that fills the window: the app shell gives
 * them a bounded `main` (`h-full`, no page scroll), each page passes that
 * height down a flex column, its message list scrolls and its composer stays
 * in view. Everything else scrolls as a page.
 *
 * The chat, and an agent's test chat (Try it → Test chat, the tab's default).
 */
export function boundedViewport(url: URL, base = ''): boolean {
	const path = url.pathname.startsWith(base) ? url.pathname.slice(base.length) : url.pathname;
	if (path === '/chat' || path.startsWith('/chat/')) return true;
	if (!/^\/agents\/[^/]+\/?$/.test(path)) return false;
	const sub = url.searchParams.get('sub');
	return url.searchParams.get('tab') === 'try' && (sub === null || sub === 'test');
}
