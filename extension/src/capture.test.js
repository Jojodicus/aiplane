/**
 * Tests for the screenshot geometry (run by `mise run test-extension`).
 *
 * Pure arithmetic, but it decides what part of someone's page ends up in an
 * image — and the gateway's own bounds check is advisory, so the same refusals
 * are pinned here.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { ELEMENT_MARGIN_PX, MAX_CAPTURE_PX, captureClip } from './capture.js';

const page = {
	scrollX: 0,
	scrollY: 500,
	viewportWidth: 1280,
	viewportHeight: 800,
	contentWidth: 1280,
	contentHeight: 3000
};

test('no area is the viewport, in document coordinates', () => {
	assert.deepEqual(captureClip({}, page), { x: 0, y: 500, width: 1280, height: 800 });
});

test('full_page is the whole document', () => {
	assert.deepEqual(captureClip({ fullPage: true }, page), {
		x: 0,
		y: 0,
		width: 1280,
		height: 3000
	});
});

test('an element is cut out with a margin, shifted by the scroll offset', () => {
	const element = { left: 100, top: 40, width: 200, height: 50 };
	assert.deepEqual(captureClip({ element }, page), {
		x: 100 - ELEMENT_MARGIN_PX,
		y: 540 - ELEMENT_MARGIN_PX,
		width: 200 + 2 * ELEMENT_MARGIN_PX,
		height: 50 + 2 * ELEMENT_MARGIN_PX
	});
});

test('a region is clamped to the page rather than capturing past its edge', () => {
	assert.deepEqual(captureClip({ region: { x: 1200, y: 2900, width: 400, height: 400 } }, page), {
		x: 1200,
		y: 2900,
		width: 80,
		height: 100
	});
});

test('an element at the top-left edge does not produce negative coordinates', () => {
	const element = { left: 2, top: -500, width: 50, height: 20 };
	const clip = captureClip({ element }, page);
	assert.equal(clip.x, 0);
	assert.equal(clip.y, 0);
});

test('two areas at once are refused', () => {
	assert.throws(
		() => captureClip({ fullPage: true, region: { x: 0, y: 0, width: 10, height: 10 } }, page),
		/one of/
	);
});

test('a region that is empty, oversized or outside the page is refused', () => {
	for (const region of [
		{ x: 0, y: 0, width: 0, height: 10 },
		{ x: 0, y: 0, width: MAX_CAPTURE_PX + 1, height: 10 },
		{ x: -5, y: 0, width: 10, height: 10 },
		{ x: 0, y: 9000, width: 10, height: 10 }
	]) {
		assert.throws(() => captureClip({ region }, page), undefined, JSON.stringify(region));
	}
});

test('a very tall page is capped, not rasterised whole', () => {
	const tall = { ...page, contentHeight: 50_000 };
	assert.equal(captureClip({ fullPage: true }, tall).height, MAX_CAPTURE_PX);
});
