<script lang="ts">
	import AiSuggestion from '$lib/components/ui/AiSuggestion.svelte';
	import type { Snippet } from 'svelte';
	import type { AgentError, AssistSuggestion } from '$lib/agents';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';

	/**
	 * One part of the prompt assistant's proposal (#117) inside the step it
	 * belongs to, until the person applies or dismisses it. Applying goes
	 * through the step's own model, so the spec is written like any other
	 * edit (and saved on Next / Apply).
	 */
	let { part, label = null, children, onapply }: {
		part: keyof AssistSuggestion['steps'];
		label?: string | null;
		children: Snippet;
		onapply: () => void | Promise<void>;
	} = $props();
	const ws = useWorkspace();

	let busy = $state(false);
	let error = $state<string | null>(null);

	async function apply() {
		busy = true;
		error = null;
		try {
			await onapply();
			ws.settle(part);
		} catch (err) {
			error = t('agents-setup-grant-failed', { reason: (err as AgentError).message ?? String(err) });
		} finally {
			busy = false;
		}
	}
</script>

{#if ws.offers(part)}
	<AiSuggestion {label}>
		{@render children()}
		{#if error}<p class="m-0 mt-2 text-error">{error}</p>{/if}
		{#snippet actions()}
			<button class="btn btn-primary btn-sm" type="button" disabled={busy} onclick={() => void apply()}>{t('agents-setup-apply')}</button>
			<button class="btn btn-ghost btn-sm" type="button" disabled={busy} onclick={() => ws.settle(part)}>{t('agents-setup-suggest-dismiss')}</button>
		{/snippet}
	</AiSuggestion>
{/if}
