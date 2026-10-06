<script lang="ts">
	import { freshName, renameKey, splitList, writerOptions, type Spec, type SpecIssue } from '#lib/agents.js';
	import { t } from '#lib/i18n.svelte.js';
	import FieldIssues from './FieldIssues.svelte';

	/**
	 * The conversation's typed state (`docs/agent-spec.md` → "State"). `set_by` is the
	 * trust boundary: only slots the model may write get a `set_<slot>` tool,
	 * and a gate can demand a slot a verifier wrote.
	 */
	let { spec = $bindable(), issues }: { spec: Spec; issues: SpecIssue[] } = $props();

	const TYPES = ['string', 'email', 'enum', 'integer', 'number', 'boolean', 'subject'];
	const writers = $derived(writerOptions(spec));

	function rename(from: string, to: string) {
		const name = to.trim();
		if (name) spec.state = renameKey(spec.state, from, name);
	}
	function toggleWriter(slot: Spec, writer: string, on: boolean) {
		const list: string[] = slot.set_by ?? [];
		slot.set_by = on ? [...list.filter((w) => w !== writer), writer] : list.filter((w) => w !== writer);
	}
	function setNumber(slot: Spec, key: string, raw: string) {
		if (raw.trim() === '') delete slot[key];
		else slot[key] = Number(raw);
	}
</script>

<div class="space-y-3">
	{#each Object.entries(spec.state as Record<string, Spec>) as [name, slot] (slot)}
		<div class="card card-border">
			<div class="card-body gap-3 p-4">
				<div class="flex flex-wrap items-end gap-3">
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-slot-name')}</span>
						<input class="input w-44 font-mono" value={name} onchange={(e) => rename(name, e.currentTarget.value)} />
					</label>
					<label class="flex flex-col gap-1">
						<span class="label-text">{t('agents-slot-type')}</span>
						<select class="select w-36" bind:value={slot.type}>
							<option value="">{t('agents-pick')}</option>
							{#each TYPES as type (type)}<option value={type}>{type}</option>{/each}
						</select>
					</label>
					<button class="btn btn-ghost btn-sm ml-auto" type="button" onclick={() => delete spec.state[name]} aria-label={t('agents-remove')}>✕</button>
				</div>
				<FieldIssues {issues} path="state.{name}" />
				<FieldIssues {issues} path="state.{name}.type" />

				<fieldset>
					<legend class="label-text mb-1">{t('agents-slot-set-by')}</legend>
					<div class="flex flex-wrap gap-x-4 gap-y-1">
						{#each writers as writer (writer)}
							<label class="label cursor-pointer gap-2">
								<input class="checkbox checkbox-xs" type="checkbox" checked={(slot.set_by ?? []).includes(writer)} onchange={(e) => toggleWriter(slot, writer, e.currentTarget.checked)} />
								<span class="font-mono text-xs">{writer}</span>
							</label>
						{/each}
					</div>
					<FieldIssues {issues} path="state.{name}.set_by" />
				</fieldset>

				<label class="flex flex-col gap-1">
					<span class="label-text">{t('agents-slot-description')}</span>
					<input class="input w-full" bind:value={slot.description} />
					<FieldIssues {issues} path="state.{name}.description" />
				</label>

				<div class="flex flex-wrap gap-3">
					{#if slot.type === 'enum'}
						<label class="flex min-w-60 flex-col gap-1">
							<span class="label-text">{t('agents-slot-values')}</span>
							<input class="input w-full" value={(slot.values ?? []).join(', ')} onchange={(e) => (slot.values = splitList(e.currentTarget.value))} placeholder={t('agents-slot-values-hint')} />
							<FieldIssues {issues} path="state.{name}.values" />
						</label>
					{/if}
					{#if slot.type === 'string'}
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-slot-min-length')}</span>
							<input class="input w-28" type="number" min="0" value={slot.min_length ?? ''} onchange={(e) => setNumber(slot, 'min_length', e.currentTarget.value)} />
							<FieldIssues {issues} path="state.{name}.min_length" />
						</label>
					{/if}
					{#if slot.type === 'string' || slot.type === 'email'}
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-slot-max-length')}</span>
							<input class="input w-28" type="number" min="0" value={slot.max_length ?? ''} onchange={(e) => setNumber(slot, 'max_length', e.currentTarget.value)} />
							<FieldIssues {issues} path="state.{name}.max_length" />
						</label>
					{/if}
					{#if slot.type === 'string'}
						<label class="flex min-w-48 flex-col gap-1">
							<span class="label-text">{t('agents-slot-pattern')}</span>
							<input class="input w-full font-mono" bind:value={slot.pattern} />
							<FieldIssues {issues} path="state.{name}.pattern" />
						</label>
					{/if}
					{#if slot.type === 'integer' || slot.type === 'number'}
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-slot-minimum')}</span>
							<input class="input w-28" type="number" value={slot.minimum ?? ''} onchange={(e) => setNumber(slot, 'minimum', e.currentTarget.value)} />
							<FieldIssues {issues} path="state.{name}.minimum" />
						</label>
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-slot-maximum')}</span>
							<input class="input w-28" type="number" value={slot.maximum ?? ''} onchange={(e) => setNumber(slot, 'maximum', e.currentTarget.value)} />
							<FieldIssues {issues} path="state.{name}.maximum" />
						</label>
					{/if}
					{#if slot.type === 'subject'}
						<p class="text-xs text-base-content/60">{t('agents-slot-subject-hint')}</p>
						<FieldIssues {issues} path="state.{name}.schema" />
					{/if}
				</div>
			</div>
		</div>
	{:else}
		<p class="text-sm text-base-content/60">{t('agents-state-empty')}</p>
	{/each}
	<button class="btn btn-sm" type="button" onclick={() => (spec.state[freshName(spec.state, 'slot')] = { type: 'string', set_by: ['llm'] })}>+ {t('agents-state-add')}</button>
</div>
