/**
 * What the /tools/browser page needs to know about the Chrome extension that
 * `browser_control` drives: where to get it, and how far a user is through
 * setting it up.
 */

import type { ExtensionStatus } from './browser-bridge';

/** The Chrome Web Store item id — the same one CI publishes to as `CWS_EXTENSION_ID`. */
const CHROME_WEB_STORE_ID = '';

export const chromeWebStoreUrl = `https://chromewebstore.google.com/detail/${CHROME_WEB_STORE_ID}`;

/**
 * The packaged extension, copied into the SPA build by
 * `mise run stage-extension`. For Chrome on Linux and for anyone who
 * installs unpacked; Windows and macOS Chrome only install from the store.
 */
export const EXTENSION_DOWNLOAD_PATH = '/downloads/aiplane-browser-control.zip';

export type BrowserSetupStage = 'not_granted' | 'tool_off' | 'not_detected' | 'switched_off' | 'ready';

/**
 * The first thing still standing between this user and a working
 * `browser_control`, in the order they have to fix them.
 *
 * `tool` is the user's `/api/v0/tools` entry for it, absent when no role
 * grants it. "Not detected" cannot tell a missing extension from an unpaired
 * one: the content script only exists on paired origins, so both are silence.
 */
export function browserSetupStage(
	tool: { enabled: boolean } | undefined,
	status: ExtensionStatus
): BrowserSetupStage {
	if (!tool) return 'not_granted';
	if (!tool.enabled) return 'tool_off';
	if (!status.present) return 'not_detected';
	if (!status.armed) return 'switched_off';
	return 'ready';
}
