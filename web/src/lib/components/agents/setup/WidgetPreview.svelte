<script lang="ts">
	import type { Spec } from '$lib/agents';
	import { readColor, readScope } from '$lib/agent-setup';
	import { parseHex, readableText } from '../../../../../shared/color.ts';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';

	/**
	 * A sketch of the website widget (`docs/embed.md`): the title it shows
	 * (its fallback when the agent has no name) and, for an example off-topic
	 * question, the answer a strict topic guard gives. It is an illustration
	 * from the spec, not a run of the agent, so it shows nothing the agent
	 * would not say.
	 */
	let { spec }: { spec: Spec } = $props();
	const ws = useWorkspace();

	const name = $derived(spec.profile?.display || ws.detail?.display || ws.detail?.name || t('embed-default-title'));
	const scope = $derived(readScope(spec));
	/** The agent's colour as the widget paints it (`web/embed/theme.ts`), text chosen for contrast. */
	const colors = $derived.by(() => {
		const rgb = parseHex(readColor(spec));
		return rgb ? `--color-primary:${readColor(spec)};--color-primary-content:${readableText(rgb)}` : undefined;
	});
</script>

<div class="w-full max-w-xs overflow-hidden rounded-box border border-base-300 bg-base-200" style={colors} aria-label={t('agents-setup-preview')}>
	<div class="flex items-center justify-between bg-primary px-3.5 py-2.5 font-bold text-primary-content">
		<span class="truncate">{name}</span><span aria-hidden="true">✕</span>
	</div>
	<div class="flex flex-col gap-2 p-3 text-sm">
		<p class="m-0 max-w-[85%] self-end rounded-box bg-primary px-2.5 py-2 text-primary-content">{t('agents-setup-preview-offtopic')}</p>
		{#if scope.strict && scope.refusal.trim()}
			<p class="m-0 max-w-[85%] rounded-box bg-base-300 px-2.5 py-2">{scope.refusal}</p>
		{:else}
			<p class="m-0 text-xs text-base-content/60">{t('agents-setup-preview-free')}</p>
		{/if}
	</div>
	<div class="flex gap-1.5 border-t border-base-300 p-2.5">
		<span class="flex-1 rounded-field bg-base-100 px-2.5 py-1.5 text-base-content/60">{t('embed-input-placeholder')}</span>
	</div>
</div>
