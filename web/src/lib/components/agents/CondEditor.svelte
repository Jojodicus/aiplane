<script lang="ts">
	import CondEditor from './CondEditor.svelte';
	import { condKind, slotValueFromText, splitList, type CondKind, type SlotInfo, type SpecIssue } from '#lib/agents.js';
	import { t } from '#lib/i18n.svelte.js';
	import FieldIssues from './FieldIssues.svelte';

	/**
	 * One node of a route's gate: `all` / `any` / `not` over children, or a
	 * leaf that checks one slot (`docs/agent-spec.md` → "Gates"). Gates are decided in
	 * code, so this editor only composes the tree; the server type-checks it
	 * against the slots and its issues land on the node by path.
	 */
	let {
		cond = $bindable(),
		slots,
		path,
		issues,
		ondelete = null
	}: {
		cond: Record<string, any>;
		slots: SlotInfo[];
		path: string;
		issues: SpecIssue[];
		ondelete?: (() => void) | null;
	} = $props();

	const kind = $derived(condKind(cond));
	const slot = $derived(slots.find((s) => s.name === cond.slot));
	const slotType = $derived(slot?.type ?? 'string');

	function setKind(next: CondKind) {
		if (next === kind) return;
		const children: Record<string, any>[] =
			kind === 'all' || kind === 'any' ? cond[kind] : kind === 'not' ? [cond.not] : [cond];
		if (next === 'all' || next === 'any') cond = { [next]: children };
		else if (next === 'not') cond = { not: children[0] ?? { slot: '', set: true } };
		else cond = children[0] && condKind(children[0]) === 'leaf' ? children[0] : { slot: '', set: true };
	}

	function text(value: unknown): string {
		return typeof value === 'string' ? value : JSON.stringify(value);
	}

	function toggle(key: 'set' | 'eq' | 'in' | 'provenance' | 'max_age', on: boolean) {
		if (!on) {
			delete cond[key];
			return;
		}
		cond[key] =
			key === 'set'
				? true
				: key === 'eq'
					? slotValueFromText(slotType, slot?.values[0] ?? '')
					: key === 'in'
						? []
						: key === 'provenance'
							? (slot?.writers[0] ?? 'llm')
							: '15m';
	}
</script>

<div class="rounded-box border border-base-300 bg-base-100 p-2">
	<div class="flex flex-wrap items-center gap-2">
		<select
			class="select select-xs w-36"
			value={kind}
			onchange={(e) => setKind(e.currentTarget.value as CondKind)}
			aria-label={t('agents-gate-kind')}
		>
			<option value="leaf">{t('agents-gate-leaf')}</option>
			<option value="all">{t('agents-gate-all')}</option>
			<option value="any">{t('agents-gate-any')}</option>
			<option value="not">{t('agents-gate-not')}</option>
		</select>
		{#if kind === 'leaf'}
			<select class="select select-xs w-44 font-mono" bind:value={cond.slot} aria-label={t('agents-gate-slot')}>
				<option value="">{t('agents-pick')}</option>
				{#each slots as s (s.name)}
					<option value={s.name}>{s.name}</option>
				{/each}
				{#if cond.slot && !slots.some((s) => s.name === cond.slot)}
					<option value={cond.slot}>{cond.slot}</option>
				{/if}
			</select>
		{/if}
		{#if ondelete}
			<button class="btn btn-ghost btn-xs ml-auto" type="button" onclick={ondelete} aria-label={t('agents-remove')}>✕</button>
		{/if}
	</div>

	{#if kind === 'leaf'}
		<div class="mt-2 grid gap-2 text-sm sm:grid-cols-2">
			<div class="flex items-center gap-2">
				<input class="checkbox checkbox-xs" type="checkbox" checked={'set' in cond} onchange={(e) => toggle('set', e.currentTarget.checked)} aria-label={t('agents-gate-check-set')} />
				<span class="w-24">{t('agents-gate-check-set')}</span>
				{#if 'set' in cond}
					<select class="select select-xs" value={String(cond.set)} onchange={(e) => (cond.set = e.currentTarget.value === 'true')} aria-label={t('agents-gate-check-set')}>
						<option value="true">{t('agents-gate-is-set')}</option>
						<option value="false">{t('agents-gate-is-unset')}</option>
					</select>
				{/if}
			</div>
			<div class="flex items-center gap-2">
				<input class="checkbox checkbox-xs" type="checkbox" checked={'eq' in cond} onchange={(e) => toggle('eq', e.currentTarget.checked)} aria-label={t('agents-gate-check-eq')} />
				<span class="w-24">{t('agents-gate-check-eq')}</span>
				{#if 'eq' in cond}
					{#if slotType === 'enum' && slot}
						<select class="select select-xs" value={text(cond.eq)} onchange={(e) => (cond.eq = e.currentTarget.value)} aria-label={t('agents-gate-check-eq')}>
							{#each slot.values as v (v)}<option value={v}>{v}</option>{/each}
						</select>
					{:else if slotType === 'boolean'}
						<select class="select select-xs" value={text(cond.eq)} onchange={(e) => (cond.eq = e.currentTarget.value === 'true')} aria-label={t('agents-gate-check-eq')}>
							<option value="true">true</option>
							<option value="false">false</option>
						</select>
					{:else}
						<input class="input input-xs w-40" value={text(cond.eq)} onchange={(e) => (cond.eq = slotValueFromText(slotType, e.currentTarget.value))} aria-label={t('agents-gate-check-eq')} />
					{/if}
				{/if}
			</div>
			<div class="flex items-center gap-2">
				<input class="checkbox checkbox-xs" type="checkbox" checked={'in' in cond} onchange={(e) => toggle('in', e.currentTarget.checked)} aria-label={t('agents-gate-check-in')} />
				<span class="w-24">{t('agents-gate-check-in')}</span>
				{#if 'in' in cond}
					<input
						class="input input-xs w-40"
						value={(cond.in as unknown[]).map(text).join(', ')}
						onchange={(e) => (cond.in = splitList(e.currentTarget.value).map((v) => slotValueFromText(slotType, v)))}
						placeholder={t('agents-gate-list-hint')}
						aria-label={t('agents-gate-check-in')}
					/>
				{/if}
			</div>
			<div class="flex items-center gap-2">
				<input class="checkbox checkbox-xs" type="checkbox" checked={'provenance' in cond} onchange={(e) => toggle('provenance', e.currentTarget.checked)} aria-label={t('agents-gate-check-provenance')} />
				<span class="w-24">{t('agents-gate-check-provenance')}</span>
				{#if 'provenance' in cond}
					<select class="select select-xs" bind:value={cond.provenance} aria-label={t('agents-gate-check-provenance')}>
						{#each slot?.writers ?? ['llm', 'host'] as w (w)}<option value={w}>{w}</option>{/each}
						{#if cond.provenance && !(slot?.writers ?? []).includes(cond.provenance)}
							<option value={cond.provenance}>{cond.provenance}</option>
						{/if}
					</select>
				{/if}
			</div>
			<div class="flex items-center gap-2">
				<input class="checkbox checkbox-xs" type="checkbox" checked={'max_age' in cond} onchange={(e) => toggle('max_age', e.currentTarget.checked)} aria-label={t('agents-gate-check-max-age')} />
				<span class="w-24">{t('agents-gate-check-max-age')}</span>
				{#if 'max_age' in cond}
					<input class="input input-xs w-24 font-mono" bind:value={cond.max_age} placeholder="15m" aria-label={t('agents-gate-check-max-age')} />
				{/if}
			</div>
		</div>
	{:else if kind === 'not'}
		<div class="mt-2 ml-3 border-l-2 border-base-300 pl-3">
			<CondEditor bind:cond={cond.not} {slots} path="{path}.not" {issues} />
		</div>
	{:else}
		<div class="mt-2 ml-3 space-y-2 border-l-2 border-base-300 pl-3">
			{#each cond[kind] as _child, i (i)}
				<CondEditor
					bind:cond={cond[kind][i]}
					{slots}
					path="{path}.{kind}[{i}]"
					{issues}
					ondelete={() => cond[kind].splice(i, 1)}
				/>
			{/each}
			<div class="flex gap-2">
				<button class="btn btn-ghost btn-xs" type="button" onclick={() => cond[kind].push({ slot: '', set: true })}>+ {t('agents-gate-add-condition')}</button>
				<button class="btn btn-ghost btn-xs" type="button" onclick={() => cond[kind].push({ all: [{ slot: '', set: true }] })}>+ {t('agents-gate-add-group')}</button>
			</div>
		</div>
	{/if}
	<FieldIssues {issues} {path} />
</div>
