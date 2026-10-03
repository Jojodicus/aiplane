<script lang="ts">
	import type { Snippet } from 'svelte';
	import { choiceCardClass } from '$lib/ui-variants';

	/**
	 * One option of a single-pick set where each option needs a sentence of
	 * explanation ("Answer on the website" / "Answer by e-mail"). Lay several
	 * out in `grid gap-2.5 sm:grid-cols-2` (or `-3`) inside a
	 * `role="radiogroup"`.
	 */
	let {
		title,
		description = null,
		selected = false,
		disabled = false,
		onselect,
		tag = null
	}: {
		title: string;
		description?: string | null;
		selected?: boolean;
		disabled?: boolean;
		onselect: () => void;
		tag?: Snippet | null;
	} = $props();
</script>

<button type="button" role="radio" aria-checked={selected} class={choiceCardClass(selected)} {disabled} onclick={onselect}>
	<span class="font-semibold">{title}</span>
	{#if description}<span class="text-sm text-base-content/60">{description}</span>{/if}
	{#if tag}<span class="mt-1">{@render tag()}</span>{/if}
</button>
