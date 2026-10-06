<script lang="ts">
	import { t } from '#lib/i18n.svelte.js';
	import { stepClass, stepState } from '#lib/ui-variants.js';

	/**
	 * Where the user is in a multi-step flow. Steps are pills: the current one
	 * filled, completed ones green, the rest muted. With `onselect` a pill
	 * jumps to its step; without, they are plain labels.
	 */
	let {
		steps,
		current,
		completed = [],
		onselect = null
	}: {
		steps: readonly string[];
		current: number;
		completed?: readonly number[];
		onselect?: ((index: number) => void) | null;
	} = $props();
</script>

<ol class="m-0 flex list-none gap-1.5 overflow-x-auto p-0 pb-1" aria-label={t('ui-steps-label')}>
	{#each steps as step, index (index)}
		{@const state = stepState(index, current, completed)}
		{@const name = state === 'done' ? t('ui-step-done', { label: step }) : step}
		<li class="flex-none">
			{#if onselect}
				<button type="button" class={stepClass(state)} aria-current={state === 'current' ? 'step' : undefined} aria-label={name} onclick={() => onselect(index)}>{step}</button>
			{:else}
				<span class="{stepClass(state)} pointer-events-none" aria-current={state === 'current' ? 'step' : undefined} aria-label={name}>{step}</span>
			{/if}
		</li>
	{/each}
</ol>
