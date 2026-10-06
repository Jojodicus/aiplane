<script lang="ts">
	import { issuesAt, type SpecIssue } from '#lib/agents.js';

	/** The validator's messages for exactly this field, shown under it. */
	let { issues, path }: { issues: SpecIssue[]; path: string } = $props();
	const messages = $derived(issuesAt(issues, path));
</script>

{#each messages as message (message)}
	<p class="mt-1 text-xs text-error" role="alert">{message}</p>
{/each}
