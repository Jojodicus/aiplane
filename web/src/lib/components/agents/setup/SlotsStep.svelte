<script lang="ts">
	import ChipToggle from '$lib/components/ui/ChipToggle.svelte';
	import StatusPill from '$lib/components/ui/StatusPill.svelte';
	import type { Spec } from '$lib/agents';
	import { SLOT_KINDS, readSlots, slotsForIdentity, writeSlots, type SlotKind, type SlotRow } from '$lib/agent-setup';
	import { t } from '$lib/i18n.svelte';
	import { writeOnChange } from './write-on-change.svelte';

	/**
	 * Information to collect: one row per state slot the model fills, as a
	 * label and a friendly kind (`SLOT_SHAPES` turns it into the slot's type
	 * and validator). Slots the identity check or the hand-offs own are not
	 * listed; a slot of a shape no kind stands for stays as it is.
	 */
	let { spec = $bindable() }: { spec: Spec } = $props();

	let rows = $state<SlotRow[]>(readSlots(spec));
	writeOnChange(() => $state.snapshot(rows), (m) => writeSlots(spec, m));

	const needed = $derived(slotsForIdentity(spec));
	const SUGGESTIONS: SlotKind[] = ['phone', 'customer_number', 'order_number', 'long_text'];
	let nextId = 0;
	const ids = new WeakMap<SlotRow, number>();
	const rowId = (row: SlotRow) => {
		if (!ids.has(row)) ids.set(row, nextId++);
		return ids.get(row) as number;
	};

	function add(kind: SlotKind, label: string) {
		rows = [...rows, { key: '', label, kind, values: [], fresh: true }];
	}
</script>

<div class="flex flex-col gap-4">
	<p class="m-0 text-base-content/70">{t('agents-setup-slots-lead')}</p>

	{#if !rows.length}
		<p class="m-0 text-sm text-base-content/60">{t('agents-setup-slots-empty')}</p>
	{/if}

	<ul class="m-0 flex list-none flex-col gap-2 p-0">
		{#each rows as row, i (rowId(row))}
			<li class="flex flex-col gap-2 rounded-box border border-base-300 bg-base-200 p-3">
				<div class="grid grid-cols-1 gap-2 sm:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)_auto] sm:items-center">
					<input class="input w-full" bind:value={row.label} aria-label={t('agents-setup-slot-label')} />
					{#if row.kind === 'custom'}
						<span class="text-sm text-base-content/60">{t('agents-setup-slot-kind-custom')}</span>
					{:else}
						<select class="select w-full" bind:value={row.kind} aria-label={t('agents-setup-slot-kind')}>
							{#each SLOT_KINDS as kind (kind)}<option value={kind}>{t(`agents-setup-slot-kind-${kind}`)}</option>{/each}
						</select>
					{/if}
					{#if needed.includes(row.key) && !row.fresh}
						<StatusPill size="sm" tone="info">{t('agents-setup-slot-in-use')}</StatusPill>
					{:else}
						<button class="btn btn-ghost btn-sm justify-self-start" type="button" aria-label={t('agents-remove')} onclick={() => (rows = rows.filter((_, j) => j !== i))}>✕</button>
					{/if}
				</div>
				{#if row.kind === 'choice'}
					<label class="flex flex-col gap-1">
						<span class="text-sm">{t('agents-setup-slot-values')}</span>
						<input
							class="input w-full"
							bind:value={() => row.values.join(', '), (v) => (row.values = v.split(',').map((x) => x.trim()))}
						/>
					</label>
				{/if}
			</li>
		{/each}
	</ul>

	<div class="flex flex-wrap items-center gap-2">
		<button class="btn btn-sm" type="button" onclick={() => add('text', t('agents-setup-slot-new'))}>{t('agents-setup-slot-add')}</button>
		<span class="text-sm text-base-content/60">{t('agents-setup-slot-suggest')}</span>
		{#each SUGGESTIONS as kind (kind)}
			<ChipToggle label="+ {t(`agents-setup-slot-kind-${kind}`)}" bind:selected={() => false, () => add(kind, t(`agents-setup-slot-kind-${kind}`))} />
		{/each}
	</div>
</div>
