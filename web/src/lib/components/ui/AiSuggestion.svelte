<script lang="ts">
	import type { Snippet } from 'svelte';
	import { t } from '$lib/i18n.svelte';

	/**
	 * Something the model proposed that the user has not accepted yet. The
	 * dashed primary edge is reserved for this, so a suggestion never reads
	 * as settled configuration. `actions` holds Apply / Dismiss.
	 */
	let { label = null, children, actions = null }: { label?: string | null; children: Snippet; actions?: Snippet | null } = $props();
</script>

<aside class="flex flex-col gap-2 rounded-box border border-dashed border-primary/60 bg-primary/5 px-3.5 py-3">
	<p class="m-0 text-xs font-bold uppercase tracking-wider text-base-content/70">
		<span class="text-primary" aria-hidden="true">✦</span>
		{label ?? t('ui-ai-suggestion')}
	</p>
	<div class="text-sm">{@render children()}</div>
	{#if actions}<div class="flex flex-wrap gap-2">{@render actions()}</div>{/if}
</aside>
