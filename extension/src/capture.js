/**
 * What part of the page a screenshot covers.
 *
 * Pure, like `policy.js`, so the rules about what ends up in an image can be
 * tested without a browser. Everything is in CSS pixels measured from the
 * top-left of the document — the space `Page.captureScreenshot`'s `clip` uses
 * once `captureBeyondViewport` is on — so a rectangle names the same content
 * however the page happens to be scrolled.
 *
 * The gateway checks the same bounds before a request is sent. That check is
 * advisory: it sits on the same side of the trust boundary as anyone with
 * script on its origin, so the refusals are repeated here.
 */

/**
 * Hard ceiling on either edge of what is captured at all, before any
 * downscaling. A page can be fifty thousand pixels tall; asking the browser to
 * rasterise that produces an image nobody will look at and can exhaust the
 * tab's memory.
 */
export const MAX_CAPTURE_PX = 8_000;

/** Room around an element, so a button is shown with its surroundings rather
 * than cropped to its border. */
export const ELEMENT_MARGIN_PX = 8;

/**
 * The clip for one screenshot.
 *
 * `page` is the tab's geometry (scroll offset, viewport, document size);
 * `element` is a `getBoundingClientRect()` in viewport coordinates.
 */
export function captureClip({ fullPage = false, region = null, element = null }, page) {
	const areas = [fullPage, Boolean(region), Boolean(element)].filter(Boolean).length;
	if (areas > 1) {
		throw new Error('a screenshot takes one of full_page, ref or region, not several');
	}

	let rect;
	if (fullPage) {
		rect = { x: 0, y: 0, width: page.contentWidth, height: page.contentHeight };
	} else if (region) {
		checkRegion(region);
		rect = region;
	} else if (element) {
		rect = {
			x: page.scrollX + element.left - ELEMENT_MARGIN_PX,
			y: page.scrollY + element.top - ELEMENT_MARGIN_PX,
			width: element.width + 2 * ELEMENT_MARGIN_PX,
			height: element.height + 2 * ELEMENT_MARGIN_PX
		};
	} else {
		rect = {
			x: page.scrollX,
			y: page.scrollY,
			width: page.viewportWidth,
			height: page.viewportHeight
		};
	}
	return clampToPage(rect, page);
}

function checkRegion({ x, y, width, height }) {
	const whole = (n) => Number.isInteger(n) && n >= 0;
	if (![x, y, width, height].every(whole)) {
		throw new Error('region x, y, width and height must be whole, non-negative CSS pixels');
	}
	if (width < 1 || height < 1 || width > MAX_CAPTURE_PX || height > MAX_CAPTURE_PX) {
		throw new Error(`region width and height must be 1-${MAX_CAPTURE_PX} CSS pixels`);
	}
}

function clampToPage(rect, page) {
	// A page shorter than its window still shows the whole window.
	const pageWidth = Math.max(page.contentWidth, page.scrollX + page.viewportWidth);
	const pageHeight = Math.max(page.contentHeight, page.scrollY + page.viewportHeight);
	const x = Math.max(0, Math.round(rect.x));
	const y = Math.max(0, Math.round(rect.y));
	const right = Math.min(pageWidth, Math.round(rect.x + rect.width));
	const bottom = Math.min(pageHeight, Math.round(rect.y + rect.height));
	const width = Math.min(right - x, MAX_CAPTURE_PX);
	const height = Math.min(bottom - y, MAX_CAPTURE_PX);
	if (width <= 0 || height <= 0) {
		throw new Error(
			`that area lies outside the page, which is ${pageWidth}×${pageHeight} CSS pixels`
		);
	}
	return { x, y, width, height };
}
