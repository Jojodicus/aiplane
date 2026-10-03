<script lang="ts">
	import AiSuggestion from '$lib/components/ui/AiSuggestion.svelte';
	import type { Snippet } from 'svelte';
	import type { AgentError, AssistSuggestion } from '$lib/agents';
	import { setupErrorMessage } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';

	/**
	 * One part of the prompt assistant's proposal (#117) inside the step it
	 * belongs to, until the person applies or dismisses it. Applying goes
	 * through the step's own model, so the spec is written like any other
	 * edit (and saved on Next / Apply). A box can stand for several parts
	 * (the abilities box: tools, knowledge bases and missing knowledge). An
	 * `onapply` that throws keeps the box, with the reason in it.
	 */
	type Part = keyof AssistSuggestion['steps'];
	let { part, label = null, children, onapply }: {
		part: Part | Part[];
		label?: string | null;
		children: Snippet;
		onapply: () => void | Promise<void>;
	} = $props();
	const ws = useWorkspace();

	const parts = $derived(Array.isArray(part) ? part : [part]);
	const settle = () => parts.forEach((p) => ws.settle(p));

	let busy = $state(false);
	let error = $state<string | null>(null);

	async function apply() {
		busy = true;
		error = null;
		try {
			await onapply();
			settle();
		} catch (err) {
			error = t('agents-setup-grant-failed', { reason: setupErrorMessage(err as AgentError, t) });
		} finally {
			busy = false;
		}
	}
</script>

{#if parts.some((p) => ws.offers(p))}
	<AiSuggestion {label}>
		{@render children()}
		{#if error}<p class="m-0 mt-2 text-error">{error}</p>{/if}
		{#snippet actions()}
			<button class="btn btn-primary btn-sm" type="button" disabled={busy} onclick={() => void apply()}>{t('agents-setup-apply')}</button>
			<button class="btn btn-ghost btn-sm" type="button" disabled={busy} onclick={settle}>{t('agents-setup-suggest-dismiss')}</button>
		{/snippet}
	</AiSuggestion>
{/if}
