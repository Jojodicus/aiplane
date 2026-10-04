<script lang="ts">
	import { slotInfos, type Granted, type Spec, type SpecIssue } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import BindsEditor from './BindsEditor.svelte';
	import FieldIssues from './FieldIssues.svelte';
	import ModelPicker from './ModelPicker.svelte';

	let { spec = $bindable(), issues, granted, ongrants }: {
		spec: Spec;
		issues: SpecIssue[];
		granted: Granted;
		ongrants: () => void;
	} = $props();

	const main = $derived(spec.main);
	const stateSlots = $derived(slotInfos(spec).map((s) => s.name));
	const toolOptions = $derived([
		...granted.tools,
		...main.tools
			.filter((id: string) => !granted.tools.some((g) => g.id === id))
			.map((id: string) => ({ id, name: id }))
	]);
	let customTool = $state('');

	function toggle(list: string[], item: string, on: boolean): string[] {
		return on ? [...list.filter((x) => x !== item), item] : list.filter((x) => x !== item);
	}
	function addCustomTool() {
		const id = customTool.trim();
		if (id && !main.tools.includes(id)) main.tools = [...main.tools, id];
		customTool = '';
	}
	function resource(tool: string): Spec {
		return (main.tool_resources[tool] ??= {});
	}
	function setModel(model: string) {
		if (model) main.model = model;
		else delete main.model;
	}
	function number(key: 'rounds' | 'seconds' | 'tokens', raw: string) {
		if (raw.trim() === '') delete main.budget[key];
		else main.budget[key] = Number(raw);
	}
</script>

<div class="space-y-4">
	<div class="flex flex-wrap items-end gap-4">
		<div class="flex min-w-60 flex-col gap-1">
			<span class="label-text">{t('agents-main-model')}</span>
			<ModelPicker
				kind="chat"
				value={main.model ?? ''}
				held={granted.models}
				resources={null}
				emptyLabel={granted.defaults?.chat ? t('agents-setup-model-default', { model: granted.defaults.chat }) : t('agents-canvas-model-default')}
				ariaLabel={t('agents-main-model')}
				onchange={setModel}
			/>
			<FieldIssues {issues} path="main.model" />
		</div>
		{#if !granted.models.length}
			<p class="text-sm text-warning">
				{t('agents-main-no-model')}
				<button class="link" type="button" onclick={ongrants}>{t('agents-tab-grants')}</button>
			</p>
		{/if}
	</div>

	<label class="flex flex-col gap-1">
		<span class="label-text">{t('agents-main-orchestration')}</span>
		<textarea class="textarea min-h-28 w-full" bind:value={main.instructions.orchestration} placeholder={t('agents-main-orchestration-hint')}></textarea>
		<FieldIssues {issues} path="main.instructions.orchestration" />
	</label>
	<label class="flex flex-col gap-1">
		<span class="label-text">{t('agents-main-response')}</span>
		<textarea class="textarea min-h-20 w-full" bind:value={main.instructions.response} placeholder={t('agents-main-response-hint')}></textarea>
		<FieldIssues {issues} path="main.instructions.response" />
	</label>

	<fieldset class="space-y-2">
		<legend class="label-text mb-1">{t('agents-main-tools')}</legend>
		{#if toolOptions.length}
			<div class="grid gap-1 sm:grid-cols-2">
				{#each toolOptions as tool (tool.id)}
					<label class="label cursor-pointer justify-start gap-2 whitespace-normal">
						<input class="checkbox checkbox-sm" type="checkbox" checked={main.tools.includes(tool.id)} onchange={(e) => (main.tools = toggle(main.tools, tool.id, e.currentTarget.checked))} />
						<span class="font-mono text-sm">{tool.name}</span>
					</label>
				{/each}
			</div>
		{:else}
			<p class="text-sm text-base-content/60">{t('agents-main-no-tools')}</p>
		{/if}
		<div class="flex gap-2">
			<input class="input w-72 font-mono" bind:value={customTool} placeholder="mcp__connector__tool" aria-label={t('agents-main-add-tool')} onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); addCustomTool(); } }} />
			<button class="btn" type="button" onclick={addCustomTool}>{t('agents-main-add-tool')}</button>
		</div>
		{#each main.tools as _tool, i (i)}<FieldIssues {issues} path="main.tools[{i}]" />{/each}
	</fieldset>

	<fieldset class="space-y-2">
		<legend class="label-text mb-1">{t('agents-main-skills')}</legend>
		{#if granted.skills.length || main.skills.length}
			<div class="grid gap-1 sm:grid-cols-2">
				{#each [...new Set([...granted.skills, ...main.skills])] as skill (skill)}
					<label class="label cursor-pointer justify-start gap-2 whitespace-normal">
						<input class="checkbox checkbox-sm" type="checkbox" checked={main.skills.includes(skill)} onchange={(e) => (main.skills = toggle(main.skills, skill, e.currentTarget.checked))} />
						<span class="text-sm">{skill}</span>
					</label>
				{/each}
			</div>
		{:else}
			<p class="text-sm text-base-content/60">{t('agents-main-no-skills')}</p>
		{/if}
		{#each main.skills as _skill, i (i)}<FieldIssues {issues} path="main.skills[{i}]" />{/each}
	</fieldset>

	{#if main.tools.length}
		<div class="space-y-2">
			<h4 class="label-text">{t('agents-main-tool-settings')}</h4>
			{#each main.tools as tool (tool)}
				<div class="collapse collapse-arrow border border-base-300">
					<input type="checkbox" aria-label={tool} />
					<div class="collapse-title font-mono text-sm">{tool}</div>
					<div class="collapse-content space-y-3">
						<label class="flex flex-col gap-1">
							<span class="label-text">{t('agents-tool-permission')}</span>
							<select class="select w-60" value={main.tool_resources[tool]?.permission ?? ''} onchange={(e) => (resource(tool).permission = e.currentTarget.value)}>
								<option value="">{t('agents-tool-permission-default')}</option>
								<option value="always_allow">{t('agents-tool-permission-allow')}</option>
								<option value="always_ask">{t('agents-tool-permission-ask')}</option>
							</select>
							<FieldIssues {issues} path="main.tool_resources.{tool}.permission" />
						</label>
						<div>
							<span class="label-text">{t('agents-tool-binds')}</span>
							<p class="mb-2 text-xs text-base-content/60">{t('agents-tool-binds-hint')}</p>
							<BindsEditor
								bind={main.tool_resources[tool]?.bind ?? {}}
								path="main.tool_resources.{tool}.bind"
								{issues}
								kinds={['state', 'route', 'const']}
								slots={stateSlots}
								onchange={(bind) => (resource(tool).bind = bind)}
							/>
						</div>
					</div>
				</div>
			{/each}
		</div>
	{/if}

	<fieldset>
		<legend class="label-text mb-1">{t('agents-main-budget')}</legend>
		<div class="flex flex-wrap gap-3">
			{#each ['rounds', 'seconds', 'tokens'] as const as key (key)}
				<label class="flex flex-col gap-1">
					<span class="text-xs text-base-content/60">{t(`agents-budget-${key}`)}</span>
					<input class="input w-28" type="number" min="1" value={main.budget[key] ?? ''} onchange={(e) => number(key, e.currentTarget.value)} />
					<FieldIssues {issues} path="main.budget.{key}" />
				</label>
			{/each}
		</div>
	</fieldset>
</div>
