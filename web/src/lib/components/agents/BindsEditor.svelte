<script lang="ts">
	import { untrack } from 'svelte';
	import { bindSource, parseBindSource, type BindKind, type SpecIssue } from '#lib/agents.js';
	import { t } from '#lib/i18n.svelte.js';
	import FieldIssues from './FieldIssues.svelte';

	/**
	 * A `bind` map: `{<parameter>: "state.<slot>" | "route.<name>" | {const}}`.
	 *
	 * The gateway fills a bound parameter and the model never sees it, so this
	 * is where a subject (whose data a tool touches) is pinned. Rows are kept
	 * locally and written back whole on every change: renaming a parameter is
	 * a rebuild of the map, which an in-place binding cannot do.
	 */
	let {
		bind,
		path,
		issues,
		kinds,
		slots = [],
		onchange
	}: {
		bind: Record<string, unknown>;
		path: string;
		issues: SpecIssue[];
		kinds: BindKind[];
		slots?: string[];
		onchange: (bind: Record<string, unknown>) => void;
	} = $props();

	type Row = { param: string; kind: BindKind; value: string };
	let rows = $state<Row[]>(
		untrack(() => Object.entries(bind).map(([param, source]) => ({ param, ...parseBindSource(source) })))
	);

	function commit() {
		onchange(
			Object.fromEntries(
				rows.filter((r) => r.param.trim()).map((r) => [r.param.trim(), bindSource(r.kind, r.value)])
			)
		);
	}
	function add() {
		rows.push({ param: '', kind: kinds[0], value: '' });
	}
	function remove(i: number) {
		rows.splice(i, 1);
		commit();
	}
	const head = (value: string) => value.split('.')[0];
	const field = (value: string) => (value.includes('.') ? value.slice(value.indexOf('.') + 1) : '');
</script>

<div class="space-y-2">
	{#each rows as row, i (i)}
		<div class="flex flex-wrap items-start gap-2">
			<input
				class="input input-sm w-40 font-mono"
				bind:value={row.param}
				onchange={commit}
				placeholder={t('agents-bind-param')}
				aria-label={t('agents-bind-param')}
			/>
			<select class="select select-sm w-28" bind:value={row.kind} onchange={commit} aria-label={t('agents-bind-source')}>
				{#each kinds as kind (kind)}
					<option value={kind}>{t(`agents-bind-kind-${kind}`)}</option>
				{/each}
			</select>
			{#if row.kind === 'state' && slots.length}
				<select
					class="select select-sm w-48 font-mono"
					value={head(row.value)}
					onchange={(e) => {
						row.value = e.currentTarget.value;
						commit();
					}}
					aria-label={t('agents-bind-value')}
				>
					<option value="">{t('agents-pick')}</option>
					{#each slots as slot (slot)}
						<option value={slot}>{slot}</option>
					{/each}
				</select>
				<input
					class="input input-sm w-36 font-mono"
					value={field(row.value)}
					onchange={(e) => {
						const sub = e.currentTarget.value.trim();
						row.value = sub ? `${head(row.value)}.${sub}` : head(row.value);
						commit();
					}}
					placeholder={t('agents-bind-field')}
					aria-label={t('agents-bind-field')}
				/>
			{:else}
				<input
					class="input input-sm w-48 font-mono"
					bind:value={row.value}
					onchange={commit}
					placeholder={t('agents-bind-value')}
					aria-label={t('agents-bind-value')}
				/>
			{/if}
			<button class="btn btn-ghost btn-sm" type="button" onclick={() => remove(i)} aria-label={t('agents-remove')}>✕</button>
		</div>
		<FieldIssues {issues} path="{path}.{row.param}" />
	{/each}
	<button class="btn btn-ghost btn-xs" type="button" onclick={add}>+ {t('agents-bind-add')}</button>
</div>
