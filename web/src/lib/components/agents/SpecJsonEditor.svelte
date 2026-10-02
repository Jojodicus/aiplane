<script lang="ts">
	import { untrack } from 'svelte';
	import { cleanSpec, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';

	/**
	 * The whole spec as JSON, for what the form does not cover (a verifier, a
	 * `human` route, a `subject` slot's schema) and for pasting a spec in. Apply
	 * replaces the form's buffer; the server still validates on save, and its
	 * issues are listed here with their paths.
	 */
	let { spec, issues, onapply }: { spec: Spec; issues: SpecIssue[]; onapply: (spec: Spec) => void } = $props();

	let text = $state(untrack(() => JSON.stringify(cleanSpec(spec), null, 2)));
	let error = $state<string | null>(null);
	let applied = $state(false);

	function apply() {
		applied = false;
		try {
			const parsed = JSON.parse(text);
			if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
				error = t('agents-json-not-object');
				return;
			}
			error = null;
			onapply(parsed);
			applied = true;
		} catch (err) {
			error = t('agents-json-invalid', { error: err instanceof Error ? err.message : String(err) });
		}
	}
</script>

<div class="space-y-3">
	<p class="text-sm text-base-content/70">{t('agents-json-hint')}</p>
	<textarea
		class="textarea min-h-[28rem] w-full font-mono text-xs"
		class:textarea-error={error}
		bind:value={text}
		oninput={() => (applied = false)}
		spellcheck="false"
		aria-label={t('agents-tab-json')}
	></textarea>
	{#if error}<div class="alert alert-error text-sm"><span>{error}</span></div>{/if}
	{#if applied}<div class="alert alert-success text-sm"><span>{t('agents-json-applied')}</span></div>{/if}
	<button class="btn btn-primary btn-sm" type="button" onclick={apply}>{t('agents-json-apply')}</button>

	{#if issues.length}
		<div class="alert alert-error alert-soft block text-sm">
			<ul class="list-inside list-disc">
				{#each issues as issue (issue.path + issue.message)}
					<li><span class="font-mono">{issue.path || t('agents-issue-root')}</span>: {issue.message}</li>
				{/each}
			</ul>
		</div>
	{/if}
</div>
