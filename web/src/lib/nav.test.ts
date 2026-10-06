import test from 'node:test';
import assert from 'node:assert/strict';
import { navItemActive } from './nav.ts';

test('a nav entry stays lit on its own sub-pages', () => {
	assert.equal(navItemActive('/admin/comfyui', '/admin/comfyui'), true);
	assert.equal(navItemActive('/admin/comfyui/jobs', '/admin/comfyui'), true);
	assert.equal(navItemActive('/rag/profiles', '/rag'), true);
	assert.equal(navItemActive('/chat/abc-123', '/chat'), true);
});

test('a sibling route that merely shares a string prefix does not light it', () => {
	// The bug a bare startsWith would introduce: two entries lit at once.
	assert.equal(navItemActive('/admin/tokens', '/settings'), false);
	assert.equal(navItemActive('/admin/skills', '/tools'), false);
	assert.equal(navItemActive('/settings-archive', '/settings'), false);
});

test('the root entry matches only itself', () => {
	assert.equal(navItemActive('/', '/'), true);
	assert.equal(navItemActive('/admin/users', '/'), false);
});

test('the shared admin access entry stays selected on all four tabs', () => {
	for (const path of ['/admin/users', '/admin/tokens', '/admin/groups', '/admin/limits']) {
		assert.equal(navItemActive(path, '/admin/users'), true, path);
	}
	assert.equal(navItemActive('/admin/settings', '/admin/users'), false);
});
