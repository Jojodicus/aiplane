import { sectionTabs, selectedSectionTab } from './section-tabs.ts';

/**
 * Sidebar highlighting.
 *
 * A nav entry stays lit on its own sub-pages: `/admin/comfyui` covers
 * `/admin/comfyui/jobs`, `/rag` covers `/rag/profiles`, `/chat` covers
 * `/chat/{id}`. Without that, opening a sub-page leaves the sidebar showing
 * nothing selected and the operator loses their place.
 *
 * The prefix must be a whole path segment — `/tokens` must not light up on
 * `/admin/tokens`, and matching on a bare `startsWith` would do exactly that
 * for any entry whose path is a string prefix of another.
 */
export function navItemActive(pathname: string, path: string): boolean {
	if (path === '/admin/users' && selectedSectionTab(pathname, sectionTabs.adminAccess)) return true;
	return pathname === path || under(pathname, path);
}

function under(pathname: string, target: string): boolean {
	// `/` is every path's prefix; it may only match itself.
	if (target === '' || target === '/') return false;
	return pathname.startsWith(`${target}/`);
}
