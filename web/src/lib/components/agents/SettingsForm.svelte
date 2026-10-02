<script lang="ts">
	import { freshName, issuesUnder, renameKey, splitList, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import FieldIssues from './FieldIssues.svelte';

	/**
	 * Everything outside the conversation itself: profile, the `finish`
	 * contract, what happens when a granted tool disappears, and the publish
	 * settings that govern embedding (`docs/agents.md` §5).
	 *
	 * `finish.schema` is a JSON-Schema subset (`docs/agents.md` §4), edited as
	 * JSON: an unsupported keyword is a save error, so a form over the subset
	 * would only restate the validator.
	 */
	let { spec = $bindable(), issues }: { spec: Spec; issues: SpecIssue[] } = $props();

	let finishText = $state(spec.finish?.schema ? JSON.stringify(spec.finish.schema, null, 2) : '');
	let finishError = $state<string | null>(null);

	function applyFinish(text: string) {
		finishText = text;
		if (!text.trim()) {
			finishError = null;
			delete spec.finish;
			return;
		}
		try {
			spec.finish = { schema: JSON.parse(text) };
			finishError = null;
		} catch (err) {
			finishError = err instanceof Error ? err.message : String(err);
		}
	}
	function setProfile(key: 'display' | 'color', value: string) {
		spec.profile ??= {};
		spec.profile[key] = value;
	}
	function publish(): Spec {
		return (spec.publish ??= {});
	}
	function patterns(): Record<string, string> {
		return ((publish().output_filter ??= {}).patterns ??= {});
	}
	function setNumber(key: string, raw: string) {
		if (raw.trim() === '') delete publish()[key];
		else publish()[key] = Number(raw);
	}
</script>

<div class="space-y-6">
	<section class="space-y-2">
		<h4 class="font-semibold">{t('agents-profile')}</h4>
		<div class="flex flex-wrap gap-3">
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-profile-display')}</span>
				<input class="input input-sm w-56" value={spec.profile?.display ?? ''} onchange={(e) => setProfile('display', e.currentTarget.value)} />
				<FieldIssues {issues} path="profile.display" />
			</label>
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-profile-color')}</span>
				<input class="input input-sm w-36 font-mono" value={spec.profile?.color ?? ''} onchange={(e) => setProfile('color', e.currentTarget.value)} placeholder="#2563eb" />
				<FieldIssues {issues} path="profile.color" />
			</label>
		</div>
	</section>

	<section class="space-y-2">
		<h4 class="font-semibold">{t('agents-verifiers')}</h4>
		<p class="text-sm text-base-content/70">{t('agents-verifiers-hint')}</p>
		<div class="flex flex-wrap gap-2">
			{#each Object.keys(spec.verifiers ?? {}) as id (id)}<span class="badge badge-outline font-mono">{id}</span>{:else}<span class="text-sm text-base-content/60">{t('agents-verifiers-none')}</span>{/each}
		</div>
		{#each issuesUnder(issues, 'verifiers') as issue (issue.path + issue.message)}
			<p class="text-xs text-error" role="alert"><span class="font-mono">{issue.path}</span>: {issue.message}</p>
		{/each}
	</section>

	<section class="space-y-2">
		<h4 class="font-semibold">{t('agents-finish')}</h4>
		<p class="text-sm text-base-content/70">{t('agents-finish-hint')}</p>
		<textarea
			class="textarea min-h-40 w-full font-mono text-xs"
			class:textarea-error={finishError}
			value={finishText}
			onchange={(e) => applyFinish(e.currentTarget.value)}
			aria-label={t('agents-finish')}
			placeholder={'{"type": "object", "required": ["answer"], "properties": {"answer": {"type": "string"}}}'}
		></textarea>
		{#if finishError}<p class="text-xs text-error" role="alert">{t('agents-json-invalid', { error: finishError })}</p>{/if}
		{#each issuesUnder(issues, 'finish') as issue (issue.path + issue.message)}
			<p class="text-xs text-error" role="alert"><span class="font-mono">{issue.path}</span>: {issue.message}</p>
		{/each}
	</section>

	<section class="space-y-2">
		<h4 class="font-semibold">{t('agents-on-unavailable')}</h4>
		<select class="select select-sm w-72" value={spec.on_tool_unavailable ?? ''} onchange={(e) => (spec.on_tool_unavailable = e.currentTarget.value)} aria-label={t('agents-on-unavailable')}>
			<option value="">{t('agents-on-unavailable-default')}</option>
			<option value="reject">{t('agents-on-unavailable-reject')}</option>
			<option value="skip">{t('agents-on-unavailable-skip')}</option>
		</select>
		<FieldIssues {issues} path="on_tool_unavailable" />
	</section>

	<section class="space-y-3">
		<h4 class="font-semibold">{t('agents-publish')}</h4>
		<p class="text-sm text-base-content/70">{t('agents-publish-hint')}</p>
		<label class="flex flex-col gap-1">
			<span class="label-text">{t('agents-publish-origins')}</span>
			<textarea
				class="textarea min-h-20 w-full max-w-xl font-mono text-xs"
				value={(spec.publish?.origins ?? []).join('\n')}
				onchange={(e) => (publish().origins = splitList(e.currentTarget.value))}
				placeholder="https://www.example.com"
			></textarea>
			<FieldIssues {issues} path="publish.origins" />
			{#each (spec.publish?.origins ?? []) as _o, i (i)}<FieldIssues {issues} path="publish.origins[{i}]" />{/each}
		</label>
		<div class="flex flex-wrap gap-3">
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-publish-idle-ttl')}</span>
				<input class="input input-sm w-28 font-mono" value={spec.publish?.idle_ttl ?? ''} onchange={(e) => (publish().idle_ttl = e.currentTarget.value)} placeholder="30m" />
				<FieldIssues {issues} path="publish.idle_ttl" />
			</label>
			<label class="flex flex-col gap-1">
				<span class="label-text">{t('agents-publish-retention')}</span>
				<input class="input input-sm w-28" type="number" min="1" value={spec.publish?.retention_days ?? ''} onchange={(e) => setNumber('retention_days', e.currentTarget.value)} />
				<FieldIssues {issues} path="publish.retention_days" />
			</label>
		</div>
		<div class="space-y-2">
			<span class="label-text">{t('agents-publish-filter')}</span>
			<p class="text-xs text-base-content/60">{t('agents-publish-filter-hint')}</p>
			{#each Object.entries(spec.publish?.output_filter?.patterns ?? {}) as [name, pattern] (name)}
				<div class="flex flex-wrap items-center gap-2">
					<input class="input input-sm w-36 font-mono" value={name} onchange={(e) => { const next = e.currentTarget.value.trim(); if (next) spec.publish.output_filter.patterns = renameKey(spec.publish.output_filter.patterns, name, next); }} aria-label={t('agents-publish-filter-name')} />
					<input class="input input-sm w-64 font-mono" value={pattern as string} onchange={(e) => (patterns()[name] = e.currentTarget.value)} aria-label={t('agents-publish-filter-regex')} />
					<button class="btn btn-ghost btn-sm" type="button" onclick={() => delete spec.publish.output_filter.patterns[name]} aria-label={t('agents-remove')}>✕</button>
				</div>
				<FieldIssues {issues} path="publish.output_filter.patterns.{name}" />
			{/each}
			<button class="btn btn-ghost btn-xs" type="button" onclick={() => (patterns()[freshName(patterns(), 'pattern')] = '')}>+ {t('agents-publish-filter-add')}</button>
		</div>
	</section>
</div>
