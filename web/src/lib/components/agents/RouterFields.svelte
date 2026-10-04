<script lang="ts">
	import { splitList, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import FieldIssues from './FieldIssues.svelte';
	import ModelPicker from './ModelPicker.svelte';

	/** The router: `rules` (first open route in order) or a `classifier` over the routes whose gate holds. */
	let { spec = $bindable(), issues, models }: { spec: Spec; issues: SpecIssue[]; models: string[] } = $props();

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
		<select class="select w-72" value={router.kind ?? ''} onchange={(e) => setRouter('kind', e.currentTarget.value)}>
			<option value="">{t('agents-router-default')}</option>
			<option value="rules">{t('agents-router-rules')}</option>
			<option value="classifier">{t('agents-router-classifier')}</option>
		</select>
		<FieldIssues {issues} path="router.kind" />
	</label>
	{#if router.kind === 'classifier'}
		<div class="flex flex-col gap-1">
			<span class="label-text">{t('agents-router-model')}</span>
			<ModelPicker
				kind="chat"
				value={router.model ?? ''}
				held={models}
				resources={null}
				emptyLabel={t('agents-router-model-main')}
				ariaLabel={t('agents-router-model')}
				class="w-56"
				onchange={(model) => setRouter('model', model)}
			/>
			<FieldIssues {issues} path="router.model" />
		</div>
	{/if}
	{#if router.kind === 'rules'}
		<label class="flex min-w-60 flex-col gap-1">
			<span class="label-text">{t('agents-router-order')}</span>
			<input
				class="input w-full font-mono"
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
