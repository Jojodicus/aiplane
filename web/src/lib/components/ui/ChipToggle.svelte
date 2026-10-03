<script lang="ts">
	import { t } from '$lib/i18n.svelte';
	import { chipClass } from '$lib/ui-variants';

	/**
	 * One option of a multi-pick set (tools, channels, tags). Toggles on
	 * click; with `onremove` it is a removable token instead and shows ✕.
	 */
	let {
		label,
		selected = $bindable(false),
		disabled = false,
		onremove = null
	}: {
		label: string;
		selected?: boolean;
		disabled?: boolean;
		onremove?: (() => void) | null;
	} = $props();
</script>

{#if onremove}
	<span class="{chipClass(true)} cursor-default">
		{label}
		<button type="button" class="opacity-60 hover:opacity-100" {disabled} aria-label={t('ui-chip-remove', { label })} onclick={onremove}>✕</button>
	</span>
{:else}
	<button type="button" class={chipClass(selected)} aria-pressed={selected} {disabled} onclick={() => (selected = !selected)}>{label}</button>
{/if}
