const SAFE_HREF = /^https?:\/\/[^\s<>"']+$/i;

/** The one link allow-list for model-authored URLs: plain http(s) with no whitespace or quote characters. */
export function isSafeHref(href: string): boolean {
	return SAFE_HREF.test(href);
}
