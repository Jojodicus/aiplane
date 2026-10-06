import test from 'node:test';
import assert from 'node:assert/strict';

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = new URL('../..', import.meta.url).pathname;
const read = (rel: string): string => readFileSync(join(ROOT, rel), 'utf8');

// svelte-check in TS 7 (`--tsgo`) mode cannot follow package.json `imports`
// to a `.svelte` file, so tsconfig repeats each entry in `paths`. An entry
// missing there still type-checks, with every component behind it as `any`.
test('every subpath import is mirrored in the tsconfig paths', () => {
	const imports: Record<string, string> = JSON.parse(read('package.json')).imports;
	const tsconfig = JSON.parse(read('tsconfig.json').replace(/^\s*\/\/.*$/gm, ''));
	const paths: Record<string, string[]> = tsconfig.compilerOptions.paths;
	assert.deepEqual(
		Object.fromEntries(Object.entries(paths).map(([key, [target]]) => [key, target])),
		imports
	);
});
