<script lang="ts">
	import { splitList, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import FieldIssues from './FieldIssues.svelte';

	/** The router: `rules` (first open route in order) or a `classifier` over the routes whose gate holds. */
	let { spec = $bindable(), issues, pools }: { spec: Spec; issues: SpecIssue[]; pools: string[] } = $props();

	const router = $derived(spec.router ?? {});

	function setRouter(key: string, value: string) {
		spec.router ??= {};
		if (value) spec.router[key] = value;
		else delete spec.router[key];
		if (!Object.keys(spec.router).length) delete spec.router;
	}
</script>

<div class="flex flex-wrap items-end gap-3">
	<label class="flex flex-col gap-1">
		<span class="label-text">{t('agents-router-kind')}</span>
		<select class="select select-sm w-72" value={router.kind ?? ''} onchange={(e) => setRouter('kind', e.currentTarget.value)}>
			<option value="">{t('agents-router-default')}</option>
			<option value="rules">{t('agents-router-rules')}</option>
			<option value="classifier">{t('agents-router-classifier')}</option>
		</select>
		<FieldIssues {issues} path="router.kind" />
	</label>
	{#if router.kind === 'classifier'}
		<label class="flex flex-col gap-1">
			<span class="label-text">{t('agents-router-pool')}</span>
			<select class="select select-sm w-48" value={router.pool ?? ''} onchange={(e) => setRouter('pool', e.currentTarget.value)}>
				<option value="">{t('agents-router-pool-main')}</option>
				{#each pools as pool (pool)}<option value={pool}>{pool}</option>{/each}
			</select>
			<FieldIssues {issues} path="router.pool" />
		</label>
	{/if}
	{#if router.kind === 'rules'}
		<label class="flex min-w-60 flex-col gap-1">
			<span class="label-text">{t('agents-router-order')}</span>
			<input
				class="input input-sm w-full font-mono"
				value={(router.order ?? []).join(', ')}
				onchange={(e) => {
					spec.router.order = splitList(e.currentTarget.value);
					if (!spec.router.order.length) delete spec.router.order;
				}}
				placeholder={t('agents-router-order-hint')}
			/>
			<FieldIssues {issues} path="router.order" />
		</label>
	{/if}
</div>
