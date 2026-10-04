import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const origin = process.env.AIPLANE_URL ?? 'http://127.0.0.1:8090';
const seedLog = process.env.DOCS_SEED_LOG ?? '/tmp/aiplane-docs-dev-ui-new.log';
const cookie = fs.readFileSync(seedLog, 'utf8').match(/^    id=(.+)$/m)?.[1];
assert.ok(cookie, 'The isolated dev-ui log must contain its synthetic admin session.');

function findPlaywright(directory) {
    const candidate = path.join(directory, 'playwright/index.mjs');
    if (fs.existsSync(candidate)) return candidate;
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        if (entry.isDirectory()) {
            const found = findPlaywright(path.join(directory, entry.name));
            if (found) return found;
        }
    }
}

const playwright = process.env.PLAYWRIGHT_DIR
    ? path.join(process.env.PLAYWRIGHT_DIR, 'index.mjs')
    : findPlaywright(path.join(os.homedir(), '.local/share/mise/installs/npm-playwright-cli'));
assert.ok(playwright, 'Install the mise Playwright tool before capturing screenshots.');
const { chromium } = await import(playwright);
const cache = path.join(os.homedir(), 'Library/Caches/ms-playwright');
let executablePath = process.env.CHROME_EXE;
if (!executablePath && fs.existsSync(cache)) {
    const revisions = fs.readdirSync(cache).filter((name) => /^chromium-\d+$/.test(name))
        .sort((a, b) => Number(b.split('-')[1]) - Number(a.split('-')[1]));
    for (const revision of revisions) {
        for (const arch of ['chrome-mac-arm64', 'chrome-mac-x64']) {
            const candidate = path.join(cache, revision, arch,
                'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing');
            if (fs.existsSync(candidate)) { executablePath = candidate; break; }
        }
        if (executablePath) break;
    }
}
const browser = await chromium.launch({ executablePath, headless: true });
try {
    const context = await browser.newContext({
        viewport: { width: 1400, height: 950 }, deviceScaleFactor: 2,
        locale: 'en-US', colorScheme: 'dark'
    });
    await context.addCookies([
        { name: 'id', value: cookie, url: origin },
        { name: 'lang', value: 'en', url: origin }
    ]);
    const page = await context.newPage();
    const sessionResponse = await context.request.get(`${origin}/api/v0/chat/sessions`);
    assert.equal(sessionResponse.status(), 200, 'Use the synthetic fixture, not a personal session.');
    const sessionsBody = await sessionResponse.json();
    const sessions = Array.isArray(sessionsBody) ? sessionsBody : sessionsBody.sessions;
    assert.ok(Array.isArray(sessions) && sessions.length, 'Screenshot fixture needs seeded conversations.');
    const conversation = sessions.find((session) => session.title === 'Has order 48217 shipped?');
    const documentConversation = sessions.find((session) => session.title === 'Draft a project brief');
    assert.ok(conversation && documentConversation, 'Both named synthetic dev-ui conversations must exist.');
    await page.goto(`${origin}/agents`);
    const agentLink = page.locator('a.card[href^="/agents/"]').filter({ hasText: 'billing-helper' }).first();
    await agentLink.waitFor();
    const agentPath = await agentLink.getAttribute('href');
    const captures = [
        { name: 'chat-overview', route: `/chat/${conversation.id}`, selector: '[data-chat-composer]', full: true },
        { name: 'chat-composer', route: `/chat/${conversation.id}`, selector: '[data-chat-composer]' },
        { name: 'capability-picker', route: `/chat/${conversation.id}`, selector: 'dialog[aria-labelledby="tool-selector-title"] .modal-box', prepare: 'tools' },
        { name: 'upstream-pool', route: '/admin/models?tab=upstreams', selector: 'article.card', text: 'wiremock-chat' },
        { name: 'automatic-routes', route: '/admin/models?tab=routing', selector: 'article.card' },
        { name: 'memory-preferences', route: '/settings/memory', selector: 'section.card', text: 'Preferences' },
        { name: 'token-card', route: '/settings/tokens', selector: 'li.card', text: 'Local laptop' },
        { name: 'browser-status', route: '/tools/browser', selector: 'section.card', text: 'Status' },
        { name: 'knowledge-collection', route: '/rag', selector: 'article.card', text: 'acme-api' },
        { name: 'scheduled-action', route: '/scheduled', selector: 'article.card', text: 'Weekly dependency report' },
        { name: 'integration-card', route: '/tools/integrations', selector: '.card', text: 'Google' },
        { name: 'content-guard', route: '/admin/settings?tab=access', selector: '[data-testid="settings-section-content_guard"]' },
        { name: 'agent-checklist', route: agentPath, selector: '[data-agent-tab] aside section.card' },
        { name: 'document-canvas', route: `/chat/${documentConversation.id}`, selector: 'aside[aria-label="Canvas"]', prepare: 'canvas' }
    ];
    fs.mkdirSync('docs/img/guide', { recursive: true });
    const manifest = [];
    for (const capture of captures) {
        const response = await page.goto(origin + capture.route, { waitUntil: 'domcontentloaded' });
        assert.equal(response.status(), 200, capture.route);
        assert.ok(!page.url().includes('/login'), 'Screenshot session must be authenticated.');
        if (capture.prepare === 'tools') {
            await page.getByTitle('Tools, integrations & skills for this conversation').first().click();
        }
        if (capture.prepare === 'canvas' && !(await page.locator(capture.selector).isVisible())) {
            await page.getByTitle('Show / hide the document canvas').click();
        }
        let target = page.locator(capture.selector);
        if (capture.text) target = target.filter({ hasText: capture.text });
        target = target.first();
        await target.waitFor({ state: 'visible' });
        await page.evaluate(() => document.fonts.ready);
        await target.scrollIntoViewIfNeeded();
        const output = `docs/img/guide/${capture.name}.png`;
        if (capture.full) await page.screenshot({ path: output });
        else await target.screenshot({ path: output });
        const size = capture.full ? { width: 1400, height: 950 } : await target.boundingBox();
        manifest.push({ file: output, route: capture.route, selector: capture.selector,
            matchingText: capture.text, viewport: { width: 1400, height: 950 }, scale: 2,
            width: Math.round(size.width * 2), height: Math.round(size.height * 2),
            data: 'Synthetic dev-ui fixture; actual application components.' });
        console.log(`${output}: ${manifest.at(-1).width}x${manifest.at(-1).height}`);
    }
    fs.writeFileSync('docs/site-support/captures.json', JSON.stringify(manifest, null, 2) + '\n');
} finally {
    await browser.close();
}
