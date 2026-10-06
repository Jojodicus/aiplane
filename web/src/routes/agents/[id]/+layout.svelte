<script lang="ts">
	import type { Snippet } from 'svelte';
	import { page } from '$app/state';
	import AgentShell from '#lib/components/agents/AgentShell.svelte';

	let { children }: { children: Snippet } = $props();
</script>

{#key page.params.id}
	<AgentShell id={page.params.id ?? ''}>{@render children()}</AgentShell>
{/key}
