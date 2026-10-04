<script lang="ts">
	import { untrack } from 'svelte';
	import { agentsApi, type AgentError, type AgentVersion } from '$lib/agents';
	import {
		FILTER_OUTCOMES,
		SECTIONS,
		blankCase,
		blankStep,
		caseBody,
		formFromCase,
		runSource,
		runState,
		type CaseForm,
		type CaseResult,
		type TestCase,
		type TestRun,
		type TestsListing
	} from '$lib/agent-tests';
	import { locale, t } from '$lib/i18n.svelte';

	/**
	 * Stored test cases and their runs (`docs/agent-builder.md` → "Evaluation"). A
	 * case is a script of visitor messages (with trusted slot writes between
	 * them) plus deterministic expectations; a run executes every case as an
	 * isolated test conversation against the saved draft or a published
	 * version and reports each as Goal, Plan and Action. A rubric is judged by
	 * a model and shown apart: it never changes pass or fail.
	 */
	let {
		agentId,
		versions,
		liveVersion,
		dirty,
		writable,
		onsave
	}: {
		agentId: string;
		versions: AgentVersion[];
		liveVersion: number | null;
		dirty: boolean;
		writable: boolean;
		onsave: () => Promise<void>;
	} = $props();

	let listing = $state<TestsListing | null>(null);
	let history = $state<TestRun[]>([]);
	let shown = $state<TestRun | null>(null);
	let source = $state<number | null>(null);
	let editing = $state<{ id: string | null; form: CaseForm } | null>(null);
	let error = $state<string | null>(null);
	let busy = $state(false);
	let running = $state(false);

	const when = (iso: string) => new Date(iso).toLocaleString(locale.current);
	const cases = $derived(listing?.cases ?? []);
	const sourceLabel = (run: TestRun) =>
		run.version === null ? t('agents-tests-source-draft') : t('agents-tests-source-version', { version: run.version });

	async function load() {
		try {
			const [tests, runs] = await Promise.all([agentsApi.tests(agentId), agentsApi.testRuns(agentId)]);
			listing = tests;
			history = runs.runs;
			if (!shown && runs.runs.length) shown = await agentsApi.testRun(agentId, runs.runs[0].id);
			error = null;
		} catch (err) {
			error = (err as AgentError).message;
		}
	}
	$effect(() => {
		void agentId;
		untrack(() => void load());
	});

	async function saveCase() {
		if (!editing) return;
		busy = true;
		error = null;
		try {
			const body = caseBody(editing.form);
			if (editing.id) await agentsApi.updateTest(agentId, editing.id, body);
			else await agentsApi.createTest(agentId, body);
			editing = null;
			await load();
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			busy = false;
		}
	}

	async function removeCase(c: TestCase) {
		if (!confirm(t('agents-tests-delete-confirm', { name: c.name }))) return;
		try {
			await agentsApi.deleteTest(agentId, c.id);
			await load();
		} catch (err) {
			error = (err as AgentError).message;
		}
	}

	async function run() {
		running = true;
		error = null;
		try {
			if (source === null && dirty) await onsave();
			shown = await agentsApi.runTests(agentId, runSource(source));
			await load();
		} catch (err) {
			error = (err as AgentError).message;
		} finally {
			running = false;
		}
	}

	async function show(id: string) {
		try {
			shown = await agentsApi.testRun(agentId, id);
		} catch (err) {
			error = (err as AgentError).message;
		}
	}

	const badge = (passed: boolean) => (passed ? 'badge-success' : 'badge-error');
	const rubricBadge = (v: string) => (v === 'passed' ? 'badge-success' : v === 'failed' ? 'badge-error' : 'badge-ghost');
	const runBadge = (r: Pick<TestRun, 'passed' | 'failed'>) => ({ green: 'badge-success', failing: 'badge-error', empty: 'badge-ghost' })[runState(r)];
	const text = (v: unknown) => (typeof v === 'string' ? v : JSON.stringify(v));
	const stepsOf = (c: TestCase) => c.script.filter((s) => 'say' in s).length;
</script>

<div class="space-y-4">
	<p class="text-sm text-base-content/70">{t('agents-tests-intro')}</p>
	{#if error}<div class="alert alert-error text-sm" role="alert"><span>{error}</span></div>{/if}

	<div class="card card-border">
		<div class="card-body gap-3 p-4">
			<div class="flex flex-wrap items-center gap-2">
				<h3 class="card-title text-base">{t('agents-tests-cases')}</h3>
				{#if writable}
					<button class="btn btn-sm ml-auto" type="button" onclick={() => (editing = { id: null, form: blankCase() })}>+ {t('agents-tests-add')}</button>
				{/if}
			</div>
			{#if cases.length === 0}
				<p class="text-sm text-base-content/60">{t('agents-tests-none')}</p>
			{:else}
				<ul class="divide-y divide-base-300">
					{#each cases as c (c.id)}
						<li class="flex flex-wrap items-center gap-2 py-2">
							<span class="font-medium">{c.name}</span>
							<span class="badge badge-ghost badge-sm">{t('agents-tests-steps', { count: stepsOf(c) })}</span>
							{#if c.rubric}<span class="badge badge-outline badge-sm">{t('agents-tests-has-rubric')}</span>{/if}
							{#if writable}
								<span class="ml-auto flex gap-1">
									<button class="btn btn-ghost btn-xs" type="button" onclick={() => (editing = { id: c.id, form: formFromCase(c) })}>{t('agents-tests-edit')}</button>
									<button class="btn btn-ghost btn-xs text-error" type="button" onclick={() => removeCase(c)}>{t('agents-delete')}</button>
								</span>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	</div>

	{#if editing}
		{@const form = editing.form}
		<div class="card card-border border-primary">
			<div class="card-body gap-4 p-4">
				<label class="flex flex-col gap-1">
					<span class="label-text">{t('agents-tests-name')}</span>
					<input class="input w-full max-w-md" bind:value={form.name} />
				</label>

				<section class="space-y-2">
					<h4 class="font-semibold">{t('agents-tests-script')}</h4>
					<p class="text-xs text-base-content/60">{t('agents-tests-script-hint')}</p>
					{#each form.steps as step, i (i)}
						<div class="flex flex-wrap items-center gap-2">
							<span class="badge badge-outline badge-sm w-24 justify-center">{step.kind === 'say' ? t('agents-tests-step-say') : t('agents-tests-step-write')}</span>
							{#if step.kind === 'say'}
								<input class="input input-sm min-w-0 flex-1" bind:value={step.text} placeholder={t('agents-tests-message')} aria-label={t('agents-tests-message')} />
							{:else}
								<input class="input input-sm w-32 font-mono" bind:value={step.slot} placeholder={t('agents-tests-slot')} aria-label={t('agents-tests-slot')} />
								<input class="input input-sm min-w-0 flex-1 font-mono" bind:value={step.value} placeholder={t('agents-tests-value')} aria-label={t('agents-tests-value')} />
								<input class="input input-sm w-36 font-mono" bind:value={step.writer} placeholder="host" aria-label={t('agents-tests-writer')} />
							{/if}
							<button class="btn btn-ghost btn-sm" type="button" onclick={() => form.steps.splice(i, 1)} aria-label={t('agents-remove')}>✕</button>
						</div>
					{/each}
					<div class="flex gap-2">
						<button class="btn btn-ghost btn-xs" type="button" onclick={() => form.steps.push(blankStep('say'))}>+ {t('agents-tests-add-say')}</button>
						<button class="btn btn-ghost btn-xs" type="button" onclick={() => form.steps.push(blankStep('write'))}>+ {t('agents-tests-add-write')}</button>
					</div>
					<p class="text-xs text-base-content/60">{t('agents-tests-writer-hint')}</p>
				</section>

				<section class="space-y-3">
					<h4 class="font-semibold">{t('agents-tests-expect')}</h4>
					<p class="text-xs text-base-content/60">{t('agents-tests-expect-hint')}</p>
					<div class="flex flex-wrap gap-3">
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-tests-finished')}</span>
							<select class="select" bind:value={form.finished}>
								<option value="">{t('agents-tests-unchecked')}</option>
								<option value="true">{t('agents-tests-finished-yes')}</option>
								<option value="false">{t('agents-tests-finished-no')}</option>
							</select>
						</label>
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-tests-filter')}</span>
							<select class="select" bind:value={form.filter}>
								<option value="">{t('agents-tests-unchecked')}</option>
								{#each FILTER_OUTCOMES as o (o)}<option value={o}>{t(`agents-tests-filter-${o}`)}</option>{/each}
							</select>
						</label>
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-tests-route')}</span>
							<select class="select" bind:value={form.routeMode}>
								<option value="">{t('agents-tests-unchecked')}</option>
								<option value="none">{t('agents-tests-route-none')}</option>
								<option value="named">{t('agents-tests-route-named')}</option>
							</select>
						</label>
						{#if form.routeMode === 'named'}
							<label class="flex flex-col gap-1">
								<span class="label-text">{t('agents-tests-route-name')}</span>
								<input class="input w-40 font-mono" bind:value={form.routeName} />
							</label>
						{/if}
					</div>

					<div class="space-y-2">
						<span class="label-text">{t('agents-tests-gates')}</span>
						{#each form.gates as gate, i (i)}
							<div class="flex flex-wrap items-center gap-2">
								<input class="input input-sm w-36 font-mono" bind:value={gate.route} placeholder={t('agents-tests-gate-route')} aria-label={t('agents-tests-gate-route')} />
								<select class="select select-sm w-28" bind:value={gate.open} aria-label={t('agents-tests-gates')}>
									<option value={true}>{t('agents-gate-open')}</option>
									<option value={false}>{t('agents-gate-closed')}</option>
								</select>
								{#if !gate.open}
									<input class="input input-sm w-56 font-mono" bind:value={gate.missing} placeholder={t('agents-tests-gate-missing')} aria-label={t('agents-tests-gate-missing')} />
								{/if}
								<button class="btn btn-ghost btn-sm" type="button" onclick={() => form.gates.splice(i, 1)} aria-label={t('agents-remove')}>✕</button>
							</div>
						{/each}
						<button class="btn btn-ghost btn-xs" type="button" onclick={() => form.gates.push({ route: '', open: true, missing: '' })}>+ {t('agents-tests-add-gate')}</button>
					</div>

					<div class="grid gap-3 md:grid-cols-2">
						{#each [['subCalled', 'agents-tests-sub-called'], ['subNotCalled', 'agents-tests-sub-not-called'], ['toolCalled', 'agents-tests-tool-called'], ['toolNotCalled', 'agents-tests-tool-not-called']] as [key, label] (key)}
							<label class="flex flex-col gap-1">
								<span class="label-text">{t(label)}</span>
								<input class="input w-full font-mono" bind:value={form[key as 'subCalled' | 'subNotCalled' | 'toolCalled' | 'toolNotCalled']} placeholder={t('agents-tests-list-hint')} />
							</label>
						{/each}
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-tests-contains')}</span>
							<textarea class="textarea min-h-16 w-full text-sm" bind:value={form.contains}></textarea>
						</label>
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-tests-not-contains')}</span>
							<textarea class="textarea min-h-16 w-full text-sm" bind:value={form.notContains}></textarea>
						</label>
					</div>

					<div class="space-y-2">
						<span class="label-text">{t('agents-tests-bound')}</span>
						{#each form.bound as b, i (i)}
							<div class="flex flex-wrap items-center gap-2">
								<input class="input input-sm w-32 font-mono" bind:value={b.route} placeholder={t('agents-tests-gate-route')} aria-label={t('agents-tests-gate-route')} />
								<input class="input input-sm w-32 font-mono" bind:value={b.name} placeholder={t('agents-tests-bound-name')} aria-label={t('agents-tests-bound-name')} />
								<input class="input input-sm w-48 font-mono" bind:value={b.value} placeholder={t('agents-tests-value')} aria-label={t('agents-tests-value')} />
								<button class="btn btn-ghost btn-sm" type="button" onclick={() => form.bound.splice(i, 1)} aria-label={t('agents-remove')}>✕</button>
							</div>
						{/each}
						<button class="btn btn-ghost btn-xs" type="button" onclick={() => form.bound.push({ route: '', name: '', value: '' })}>+ {t('agents-tests-add-bound')}</button>
					</div>
				</section>

				<label class="flex flex-col gap-1">
					<span class="label-text">{t('agents-tests-rubric-field')}</span>
					<textarea class="textarea min-h-16 w-full text-sm" bind:value={form.rubric}></textarea>
					<span class="text-xs text-base-content/60">{t('agents-tests-rubric-hint')}</span>
				</label>

				<div class="flex gap-2">
					<button class="btn btn-primary btn-sm" type="button" disabled={busy} onclick={saveCase}>{t('agents-tests-save')}</button>
					<button class="btn btn-ghost btn-sm" type="button" onclick={() => (editing = null)}>{t('agents-tests-cancel')}</button>
				</div>
			</div>
		</div>
	{/if}

	<div class="card card-border">
		<div class="card-body gap-3 p-4">
			<div class="flex flex-wrap items-end gap-3">
				<h3 class="card-title text-base">{t('agents-tests-results')}</h3>
				{#if writable}
					<label class="fieldset ml-auto">
						<span class="fieldset-legend text-xs">{t('agents-tests-source')}</span>
						<select class="select select-sm" bind:value={source}>
							<option value={null}>{t('agents-tests-source-draft')}</option>
							{#each versions as v (v.version)}
								<option value={v.version}>{t('agents-tests-source-version', { version: v.version })}{v.version === liveVersion ? ` · ${t('agents-live')}` : ''}</option>
							{/each}
						</select>
					</label>
					<button class="btn btn-primary btn-sm" type="button" disabled={running || cases.length === 0} onclick={run}>
						{#if running}<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>{/if}
						{running ? t('agents-tests-running') : t('agents-tests-run')}
					</button>
				{/if}
			</div>

			{#if listing?.latest_draft_run && !listing.latest_draft_run_current}
				<div class="alert alert-warning text-sm"><span>{t('agents-tests-stale')}</span></div>
			{/if}

			{#if shown}
				<div class="flex flex-wrap items-center gap-2">
					<span class="badge {runBadge(shown)}">{t(`agents-tests-state-${runState(shown)}`)}</span>
					<span class="text-sm">{t('agents-tests-summary', { passed: shown.passed, failed: shown.failed })}</span>
					<span class="text-sm text-base-content/60">{sourceLabel(shown)} · {when(shown.started_at)}</span>
					{#if shown.rubric && shown.rubric.passed + shown.rubric.failed + shown.rubric.error + shown.rubric.skipped > 0}
						<span class="badge badge-outline badge-sm">{t('agents-tests-rubric-summary', { passed: shown.rubric.passed, failed: shown.rubric.failed })}</span>
					{/if}
				</div>
				{#each shown.results ?? [] as r (r.case_id)}
					{@render result(r)}
				{/each}
			{:else}
				<p class="text-sm text-base-content/60">{t('agents-tests-no-runs')}</p>
			{/if}

			{#if history.length > 1}
				<details>
					<summary class="cursor-pointer text-sm font-semibold">{t('agents-tests-history')}</summary>
					<ul class="mt-2 space-y-1">
						{#each history as h (h.id)}
							<li>
								<button class="btn btn-ghost btn-xs justify-start gap-2" type="button" onclick={() => show(h.id)}>
									<span class="badge badge-sm {runBadge(h)}">{t('agents-tests-summary', { passed: h.passed, failed: h.failed })}</span>
									{sourceLabel(h)} · {when(h.started_at)}
								</button>
							</li>
						{/each}
					</ul>
				</details>
			{/if}
		</div>
	</div>
</div>

{#snippet result(r: CaseResult)}
	<details class="collapse collapse-arrow border border-base-300" open={!r.passed}>
		<summary class="collapse-title flex items-center gap-2 py-2 text-sm font-medium">
			<span class="badge badge-sm {badge(r.passed)}">{r.passed ? t('agents-tests-passed') : t('agents-tests-failed')}</span>
			{r.case_name}
		</summary>
		<div class="collapse-content space-y-3 text-sm">
			{#if r.report.error}<div class="alert alert-error text-sm" role="alert"><span>{t('agents-tests-stopped')} {r.report.error}</span></div>{/if}
			<div class="grid gap-3 lg:grid-cols-3">
				{#each SECTIONS as name (name)}
					{@const section = r.report[name]}
					<div class="rounded-box border border-base-300 p-3">
						<div class="flex items-center gap-2">
							<span class="font-semibold">{t(`agents-tests-section-${name}`)}</span>
							<span class="badge badge-sm {badge(section.passed)}">{section.passed ? t('agents-tests-passed') : t('agents-tests-failed')}</span>
						</div>
						<p class="text-xs text-base-content/60">{t(`agents-tests-section-${name}-desc`)}</p>
						{#if section.checks.length === 0}
							<p class="mt-2 text-xs text-base-content/50">{t('agents-tests-no-checks')}</p>
						{:else}
							<ul class="mt-2 space-y-1">
								{#each section.checks as c, i (i)}
									<li class="flex gap-2">
										<span class={c.passed ? 'text-success' : 'text-error'} aria-hidden="true">{c.passed ? '✓' : '✗'}</span>
										<span>
											{c.message}
											{#if !c.passed}<span class="block font-mono text-xs text-base-content/60">{t('agents-tests-expected')} {text(c.expected)} · {t('agents-tests-actual')} {text(c.actual)}</span>{/if}
										</span>
									</li>
								{/each}
							</ul>
						{/if}
					</div>
				{/each}
			</div>
			{#if r.report.rubric}
				<div class="rounded-box border border-dashed border-base-300 p-3">
					<div class="flex items-center gap-2">
						<span class="font-semibold">{t('agents-tests-rubric')}</span>
						<span class="badge badge-sm {rubricBadge(r.report.rubric.verdict)}">{t(`agents-tests-rubric-${r.report.rubric.verdict}`)}</span>
					</div>
					<p class="text-xs text-base-content/60">{t('agents-tests-rubric-note')}</p>
					<p class="mt-1">{r.report.rubric.reason}</p>
				</div>
			{/if}
			{#if r.report.turns.length}
				<div class="space-y-1">
					<h4 class="font-semibold">{t('agents-tests-conversation')}</h4>
					{#each r.report.turns as turn, i (i)}
						<p><span class="font-medium">{t('agents-test-visitor')}:</span> {turn.message}</p>
						<p class="ml-4"><span class="font-medium">{t('agents-test-agent')}:</span> {turn.answer ?? turn.error ?? ''}</p>
					{/each}
				</div>
			{/if}
		</div>
	</details>
{/snippet}
