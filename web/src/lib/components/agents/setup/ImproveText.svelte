<script lang="ts">
	import AiSuggestion from '$lib/components/ui/AiSuggestion.svelte';
	import { agentsApi, type AgentError, type ImproveField } from '$lib/agents';
	import { setupErrorMessage } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';

	/**
	 * "Improve" for one text (the task, the tone, the answer for other
	 * topics): the prompt assistant (#117, `assist/improve`) proposes a better
	 * wording, shown before / after with its reason, and nothing changes
	 * until the person applies it.
	 */
	let { field, text, onapply }: { field: ImproveField; text: string; onapply: (suggestion: string) => void } = $props();
	const ws = useWorkspace();

	let busy = $state(false);
	let error = $state<string | null>(null);
	let proposal = $state<{ before: string; after: string; why: string } | null>(null);

	async function ask() {
		busy = true;
		error = null;
		const before = text;
		try {
			const r = await agentsApi.improve(ws.id, field, before);
			proposal = { before, after: r.suggestion, why: r.why };
		} catch (err) {
			error = setupErrorMessage(err as AgentError, t, 'assist');
		} finally {
			busy = false;
		}
	}
</script>

<div class="flex flex-col gap-2">
	<button class="btn btn-ghost btn-sm self-start" type="button" disabled={busy || !text.trim() || !ws.writable} onclick={() => void ask()}>
		{#if busy}<span class="loading loading-spinner loading-xs"></span>{:else}<span class="text-primary" aria-hidden="true">✦</span>{/if}
		{t('agents-setup-improve')}
	</button>
	{#if error}<p class="m-0 text-sm text-error" role="alert">{error}</p>{/if}
	{#if proposal}
		<AiSuggestion>
			<div class="grid gap-2.5 sm:grid-cols-2">
				<div class="min-w-0 rounded-field bg-base-200 p-2.5">
					<p class="m-0 text-xs font-semibold uppercase tracking-wider text-base-content/60">{t('agents-setup-improve-before')}</p>
					<p class="m-0 mt-1 whitespace-pre-line break-words">{proposal.before}</p>
				</div>
				<div class="min-w-0 rounded-field bg-base-200 p-2.5">
					<p class="m-0 text-xs font-semibold uppercase tracking-wider text-base-content/60">{t('agents-setup-improve-after')}</p>
					<p class="m-0 mt-1 whitespace-pre-line break-words">{proposal.after}</p>
				</div>
			</div>
			{#if proposal.why}<p class="m-0 mt-2 text-base-content/70">{proposal.why}</p>{/if}
			{#snippet actions()}
				<button class="btn btn-primary btn-sm" type="button" onclick={() => { onapply(proposal?.after ?? ''); proposal = null; }}>{t('agents-setup-apply')}</button>
				<button class="btn btn-ghost btn-sm" type="button" onclick={() => (proposal = null)}>{t('agents-setup-suggest-dismiss')}</button>
			{/snippet}
		</AiSuggestion>
	{/if}
</div>
