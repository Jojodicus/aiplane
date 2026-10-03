<script lang="ts">
	import { onMount } from 'svelte';
	import { agentsApi, type Spec } from '$lib/agents';
	import { deriveBind, identityWriter, readHandoffs, writeHandoffs, type Rule } from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/**
	 * Hand-offs as sentences: "When it's about X and Y, hand over to Z". The
	 * route, its gate, its task and what it passes the specialist are derived
	 * (`writeHandoffs`, `deriveBind`); routes of any other shape are kept.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();

	let model = $state(readHandoffs(spec));
	writeOnChange(() => $state.snapshot(model), (m) => writeHandoffs(spec, m));

	const hasIdentity = $derived(identityWriter(spec) !== null);
	/** Per rule index: what the specialist needs that cannot be supplied, or `null` when its setup was unreadable. */
	let problems = $state<Record<number, string[] | null>>({});

	async function derive(i: number) {
		const rule = model.rules[i];
		if (!rule || rule.target.kind !== 'agent' || !rule.target.id) {
			if (rule) rule.bind = {};
			delete problems[i];
			return;
		}
		try {
			const specialist = await agentsApi.get(rule.target.id);
			const { bind, missing } = deriveBind(specialist.live_spec ?? specialist.draft_spec, spec);
			rule.bind = bind;
			if (Object.keys(bind).length && hasIdentity) rule.identity = true;
			problems[i] = missing;
		} catch {
			problems[i] = null;
		}
	}

	onMount(() => model.rules.forEach((_, i) => void derive(i)));

	const targetValue = (rule: Rule) => (rule.target.kind === 'human' ? 'human' : `agent:${rule.target.id}`);
	function setTarget(i: number, value: string) {
		model.rules[i].target = value === 'human' ? { kind: 'human' } : { kind: 'agent', id: value.slice(6) };
		void derive(i);
	}

	function add() {
		model.rules = [...model.rules, { route: null, topic: '', identity: false, target: { kind: 'human' }, bind: {} }];
	}
	function remove(i: number) {
		model.rules = model.rules.filter((_, j) => j !== i);
		problems = {};
		model.rules.forEach((_, j) => void derive(j));
	}
</script>

<div class="flex flex-col gap-4">
	<p class="m-0 text-base-content/70">{t('agents-setup-routes-lead')}</p>

	<ul class="m-0 flex list-none flex-col gap-2 p-0">
		{#each model.rules as rule, i (i)}
			<li class="flex flex-col gap-2 rounded-box border border-base-300 bg-base-200 p-3">
				<div class="flex flex-wrap items-center gap-2 font-semibold">
					<span class="font-normal text-base-content/60">{t('agents-setup-rule-when')}</span>
					<input class="input w-48 max-w-full" bind:value={rule.topic} placeholder={t('agents-setup-rule-topic-placeholder')} aria-label={t('agents-setup-rule-topic')} />
					<span class="font-normal text-base-content/60">{t('agents-setup-rule-and')}</span>
					<select
						class="select w-auto max-w-full"
						aria-label={t('agents-setup-rule-condition')}
						bind:value={() => (rule.identity ? 'verified' : 'always'), (v) => (rule.identity = v === 'verified')}
						disabled={Object.keys(rule.bind).length > 0 && rule.identity}
					>
						<option value="always">{t('agents-setup-rule-always')}</option>
						<option value="verified" disabled={!hasIdentity}>{t('agents-setup-rule-verified')}</option>
					</select>
					<span class="font-normal text-base-content/60">{t('agents-setup-rule-then')}</span>
					<select class="select w-auto max-w-full" aria-label={t('agents-setup-rule-target')} value={targetValue(rule)} onchange={(e) => setTarget(i, e.currentTarget.value)}>
						<option value="human">{t('agents-setup-rule-person')}</option>
						{#each ws.agents as agent (agent.id)}
							<option value="agent:{agent.id}">{t('agents-setup-rule-agent', { name: agent.display || agent.name })}</option>
						{/each}
						{#if rule.target.kind === 'agent' && !rule.target.id}<option value="agent:" disabled>{t('agents-pick')}</option>{/if}
					</select>
					<button class="btn btn-ghost btn-sm ml-auto" type="button" aria-label={t('agents-remove')} onclick={() => remove(i)}>✕</button>
				</div>
				{#if !hasIdentity}
					<p class="m-0 text-xs text-base-content/60">{t('agents-setup-rule-no-identity')}</p>
				{/if}
				{#if Object.keys(rule.bind).length && rule.identity}
					<p class="m-0 text-xs text-base-content/60">{t('agents-setup-rule-needs-identity')}</p>
				{/if}
				{#if problems[i] === null}
					<p class="m-0 text-xs text-warning">{t('agents-setup-rule-unreadable')}</p>
				{:else if problems[i]?.length}
					<p class="m-0 text-xs text-warning">{t('agents-setup-rule-bind-missing', { names: (problems[i] ?? []).join(', ') })}</p>
				{/if}
			</li>
		{/each}
	</ul>
	<button class="btn btn-sm self-start" type="button" onclick={add}>{t('agents-setup-rule-add')}</button>

	<div class="flex flex-wrap items-center gap-2 rounded-box border border-base-300 bg-base-200 p-3 font-semibold">
		<span class="font-normal text-base-content/60">{t('agents-setup-fallback')}</span>
		<select class="select w-auto max-w-full" aria-label={t('agents-setup-fallback')} bind:value={() => (model.fallback ? 'human' : 'none'), (v) => (model.fallback = v === 'human')}>
			<option value="human">{t('agents-setup-fallback-human')}</option>
			<option value="none">{t('agents-setup-fallback-none')}</option>
		</select>
	</div>

	{#if model.custom.length}
		<p class="m-0 text-sm text-base-content/60">{t('agents-setup-routes-custom', { count: model.custom.length })}</p>
	{/if}
	<p class="m-0 text-sm text-base-content/60">{t('agents-setup-routes-note')}</p>
</div>
