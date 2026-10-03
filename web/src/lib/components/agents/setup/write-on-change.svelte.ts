import { untrack } from 'svelte';

/**
 * Writes a step's model into the spec whenever the person changes it, and
 * not before: merely opening a step must not mark the draft as changed, even
 * where writing back would normalise the spec's JSON (key order, trimming).
 * Call it during component setup; `model` returns a plain snapshot.
 */
export function writeOnChange<M>(model: () => M, write: (m: M) => void): void {
	let last = JSON.stringify(model());
	$effect(() => {
		const json = JSON.stringify(model());
		if (json === last) return;
		last = json;
		untrack(() => write(JSON.parse(json) as M));
	});
}
