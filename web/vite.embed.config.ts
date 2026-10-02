import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

/**
 * The embeddable agent widget (issue #94): one self-contained IIFE,
 * `embed.js`, deliberately separate from the SPA build. It shares nothing with
 * `web/src` except the generated Fluent catalogs' `embed-*` keys, so a host
 * page downloads the widget and not the app.
 *
 * Written into the SPA's output directory by `mise run build-web`, which is
 * where the gateway serves it from (`/embed.js`).
 */
export default defineConfig({
	plugins: [tailwindcss()],
	build: {
		outDir: '../target/frontend/build',
		emptyOutDir: false,
		copyPublicDir: false,
		target: 'es2022',
		lib: {
			entry: 'embed/main.ts',
			name: 'CroitAiplaneEmbed',
			formats: ['iife'],
			fileName: () => 'embed.js'
		}
	}
});
