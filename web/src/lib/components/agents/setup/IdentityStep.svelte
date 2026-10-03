<script lang="ts">
	import ChoiceCard from '$lib/components/ui/ChoiceCard.svelte';
	import type { Spec } from '$lib/agents';
	import {
		IDENTITY_METHODS,
		newSecret,
		readIdentity,
		suggestedMethod,
		topicsNeedingIdentity,
		writeIdentity,
		type IdentityMethod
	} from '$lib/agent-setup';
	import { useWorkspace } from '$lib/agent-workspace.svelte';
	import { t } from '$lib/i18n.svelte';
	import SuggestionBox from './SuggestionBox.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/**
	 * How the agent learns who is writing, as four cards. Each writes the
	 * `identity` verifier (`mcp_code`, `host_jwt` or `lookup`) and the
	 * `verified` slot it fills; the hand-off rules' "identity confirmed"
	 * follows whichever writer that is.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();
	const ws = useWorkspace();

	let model = $state(readIdentity(spec));
	writeOnChange(
		() => $state.snapshot(model),
		(m) => writeIdentity(spec, m, { email: t('agents-tpl-slot-email'), name: t('agents-tpl-slot-name'), customerNumber: t('agents-setup-slot-kind-customer_number') })
	);

	const blockers = $derived(topicsNeedingIdentity(spec));
	const connectors = $derived(ws.resources?.connectors ?? []);
	const lookupTools = $derived([
		...(ws.resources?.tools ?? []).map((x) => ({ id: x.id, name: x.name })),
		...connectors.flatMap((c) => c.tools.map((id) => ({ id, name: `${c.name}: ${id.split('__').pop()}` })))
	]);

	let generated = $state(false);

	function pick(kind: 'connector' | 'tool', ref: string) {
		const previous = model[kind];
		model[kind] = ref;
		const grantOf = (r: string) => {
			const connector = kind === 'connector' ? r : connectors.find((c) => c.tools.includes(r))?.key;
			return connector ? { kind: 'connector' as const, ref: connector } : { kind: 'tool' as const, ref: r };
		};
		if (ref) {
			const g = grantOf(ref);
			ws.stageGrant(g.kind, g.ref);
		}
		if (previous && previous !== ref) {
			const g = grantOf(previous);
			if (!(abilityUses(g.kind, g.ref))) ws.stageRevoke(g.kind, g.ref);
		}
	}
	/** A connector or tool the agent's abilities also use stays granted when the identity check lets go of it. */
	function abilityUses(kind: 'connector' | 'tool', ref: string): boolean {
		const tools: string[] = spec.main?.tools ?? [];
		return kind === 'tool' ? tools.includes(ref) : tools.some((id) => id.startsWith(`mcp__${ref}__`));
	}

	const suggested = $derived(ws.suggestion?.steps.identity ?? null);
	const suggestedCard = $derived(suggested ? suggestedMethod(suggested.method) : null);

	function choose(method: IdentityMethod) {
		model.method = method;
	}
</script>

<div class="flex flex-col gap-5">
	<p class="m-0 text-base-content/70">{t('agents-setup-identity-lead')}</p>
	{#if suggested && suggestedCard && suggestedCard !== model.method}
		<SuggestionBox part="identity" onapply={() => { if (suggestedCard && !(suggestedCard === 'none' && blockers.length)) choose(suggestedCard); }}>
			<p class="m-0 font-semibold">{t(`agents-setup-identity-${suggestedCard}`)}</p>
			<p class="m-0 mt-1 text-base-content/70">{suggested.why}</p>
		</SuggestionBox>
	{/if}

	<div class="grid gap-2.5 sm:grid-cols-2" role="radiogroup" aria-label={t('agents-setup-step-identity')}>
		{#each IDENTITY_METHODS as method (method)}
			<ChoiceCard
				title={t(`agents-setup-identity-${method}`)}
				description={t(`agents-setup-identity-${method}-desc`)}
				selected={model.method === method}
				disabled={method === 'none' && blockers.length > 0}
				onselect={() => choose(method)}
			/>
		{/each}
	</div>
	{#if blockers.length && model.method !== 'none'}
		<p class="m-0 text-sm text-base-content/60">{t('agents-setup-identity-blocked', { topic: blockers.join(', ') })}</p>
	{/if}
	{#if model.others}
		<p class="m-0 text-sm text-base-content/60">{t('agents-setup-identity-custom', { count: model.others })}</p>
	{/if}

	{#if model.method === 'email_code'}
		<label class="flex flex-col gap-1">
			<span class="font-semibold">{t('agents-setup-identity-connector')}</span>
			{#if connectors.length || model.connector}
				<select class="select w-full max-w-sm" value={model.connector} onchange={(e) => pick('connector', e.currentTarget.value)}>
					<option value="">{t('agents-pick')}</option>
					{#each connectors as c (c.key)}<option value={c.key}>{c.name}</option>{/each}
					{#if model.connector && !connectors.some((c) => c.key === model.connector)}<option value={model.connector}>{model.connector}</option>{/if}
				</select>
				<span class="text-sm text-base-content/60">{t('agents-setup-identity-connector-hint')}</span>
			{:else}
				<span class="text-sm text-base-content/60">{t('agents-setup-identity-no-connector')}</span>
			{/if}
		</label>
	{:else if model.method === 'customer_lookup'}
		<label class="flex flex-col gap-1">
			<span class="font-semibold">{t('agents-setup-identity-tool')}</span>
			<select class="select w-full max-w-sm" value={model.tool} onchange={(e) => pick('tool', e.currentTarget.value)}>
				<option value="">{t('agents-pick')}</option>
				{#each lookupTools as tool (tool.id)}<option value={tool.id}>{tool.name}</option>{/each}
				{#if model.tool && !lookupTools.some((x) => x.id === model.tool)}<option value={model.tool}>{model.tool}</option>{/if}
			</select>
			<span class="text-sm text-base-content/60">{t('agents-setup-identity-tool-hint')}</span>
		</label>
	{:else if model.method === 'signed_in'}
		<fieldset class="flex flex-col gap-3 rounded-box border border-base-300 p-4">
			<legend class="px-1 font-semibold">{t('agents-setup-identity-dev')}</legend>
			<label class="flex flex-col gap-1">
				<span class="text-sm">{t('agents-setup-identity-issuer')}</span>
				<input class="input w-full" bind:value={model.issuer} placeholder="https://www.example.com" />
			</label>
			<label class="flex flex-col gap-1">
				<span class="text-sm">{t('agents-setup-identity-audience')}</span>
				<input class="input w-full" bind:value={model.audience} placeholder="support-agent" />
			</label>
			<label class="flex flex-col gap-1">
				<span class="text-sm">{t('agents-setup-identity-secret')}</span>
				<div class="flex flex-wrap gap-2">
					<input class="input min-w-0 flex-1 font-mono" type={generated ? 'text' : 'password'} autocomplete="off" bind:value={model.secret} minlength="32" />
					<button class="btn" type="button" onclick={() => ((model.secret = newSecret()), (generated = true))}>{t('agents-setup-identity-secret-generate')}</button>
				</div>
				<span class="text-sm text-base-content/60">
					{model.secretSet && !model.secret ? t('agents-setup-identity-secret-set') : t('agents-setup-identity-secret-hint')}
				</span>
			</label>
		</fieldset>
	{/if}
</div>
