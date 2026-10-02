<script lang="ts">
	import { page } from '$app/state';
	import AgentWorkbench from '$lib/components/agents/AgentWorkbench.svelte';
</script>

{#key page.params.id}
	<AgentWorkbench id={page.params.id ?? ''} />
{/key}
