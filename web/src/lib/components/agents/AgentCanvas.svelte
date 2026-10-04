<script lang="ts">
	import { tick } from 'svelte';
	import { base } from '$app/paths';
	import {
		edgeOnPath,
		edgePath,
		describeCond,
		issuesByNode,
		layoutCanvas,
		summarizeMain,
		testPath,
		type CanvasNode
	} from '$lib/agent-canvas';
	import { addRoute, removeRoute, type AgentSummary, type Granted, type Spec, type SpecIssue, type TestDebug } from '$lib/agents';
	import { t } from '$lib/i18n.svelte';
	import MainAgentForm from './MainAgentForm.svelte';
	import RouteEditor from './RouteEditor.svelte';
	import RouterFields from './RouterFields.svelte';
	import StateSlotsForm from './StateSlotsForm.svelte';

	/**
	 * The agent's fixed topology (`docs/agents.md`): main agent, then per route
	 * a gate, the route and what it opens. Not a free-form graph: every node is
	 * a view of the spec, and every edit goes through the same buffer the form
	 * builder and the JSON tab bind, so there is one source of truth. Selecting
	 * a node opens the form editors for that part of the spec beside it.
	 */
	let { spec = $bindable(), issues, granted, agents, lastDebug, ongrants }: {
		spec: Spec;
		issues: SpecIssue[];
		granted: Granted;
		agents: AgentSummary[];
		lastDebug: TestDebug | null;
		ongrants: () => void;
	} = $props();

	const graph = $derived(layoutCanvas(spec));
	const byId = $derived(new Map(graph.nodes.map((n) => [n.id, n])));
	const problems = $derived(issuesByNode(spec, issues));
	const path = $derived(testPath(lastDebug));
	const main = $derived(summarizeMain(spec));

	let selectedId = $state<string | null>(null);
	const selected = $derived(selectedId ? (byId.get(selectedId) ?? null) : null);
	let heading = $state<HTMLElement | null>(null);
	let canvasEl = $state<HTMLElement | null>(null);

	const nodeEl = (id: string) => canvasEl?.querySelector<HTMLElement>(`[data-node="${CSS.escape(id)}"]`);

	/** The panel narrows the canvas, which can push the chosen node out of its scroll area. */
	async function select(id: string) {
		selectedId = id;
		await tick();
		heading?.focus();
		const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
		nodeEl(id)?.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: still ? 'instant' : 'smooth' });
	}

	async function close() {
		const id = selectedId;
		selectedId = null;
		await tick();
		if (id) nodeEl(id)?.focus();
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.key === 'Escape' && selected) {
			e.preventDefault();
			void close();
		}
	}

	function add() {
		void select(`route:${addRoute(spec)}`);
	}

	function remove(name: string) {
		removeRoute(spec, name);
		selectedId = null;
	}

	const agentOf = (id: string | undefined) => agents.find((a) => a.id === id);

	function title(node: CanvasNode): string {
		if (node.kind === 'main') return t('agents-canvas-kind-main');
		const route = spec.routes[node.route!];
		if (node.kind === 'target') {
			if (node.targetKind === 'agent') {
				const a = agentOf(route.agent);
				return a ? a.display || a.name : route.agent || t('agents-canvas-no-agent');
			}
			if (node.targetKind === 'human') return route.human?.inbox || t('agents-canvas-target-human');
			return node.targetKind ?? '';
		}
		return node.route!;
	}

	function kindLabel(node: CanvasNode): string {
		if (node.kind !== 'target') return t(`agents-canvas-kind-${node.kind}`);
		if (node.targetKind === 'agent') return t('agents-canvas-target-agent');
		if (node.targetKind === 'human') return t('agents-canvas-target-human');
		return t('agents-canvas-target-other', { kind: node.targetKind ?? '' });
	}

	function tone(node: CanvasNode): string {
		switch (node.kind) {
			case 'main':
				return 'bg-primary/10 border-primary/50';
			case 'gate':
				return 'bg-warning/10 border-warning/50';
			case 'route':
				return 'bg-base-200 border-base-300';
			default:
				return node.targetKind === 'agent'
					? 'bg-secondary/10 border-secondary/50'
					: node.targetKind === 'human'
						? 'bg-accent/10 border-accent/50'
						: 'bg-base-100 border-base-content/30 border-dashed';
		}
	}

	function onPath(node: CanvasNode): boolean {
		return !!path && !!node.route && (node.route === path.picked || node.route in path.dispatched);
	}

	const gateText = (route: string) => describeCond(spec.routes[route]?.when, t('agents-canvas-gate-set'));
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="space-y-3" {onkeydown}>
	<div class="flex flex-wrap items-center gap-3">
		<p class="min-w-60 flex-1 text-sm text-base-content/70">{t('agents-canvas-intro')}</p>
		<button class="btn btn-sm" type="button" onclick={add}>+ {t('agents-routes-add')}</button>
	</div>
	<p class="text-xs text-base-content/60" aria-live="polite">
		{#if !path}
			{t('agents-canvas-no-test')}
		{:else if path.picked}
			{t('agents-canvas-last-turn', { route: path.picked })}
		{:else}
			{t('agents-canvas-last-turn-none')}
		{/if}
	</p>

	<div class="grid items-start gap-4 {selected ? 'xl:grid-cols-[minmax(0,1fr)_26rem]' : ''}">
		<div class="min-w-0 overflow-x-auto rounded-box border border-base-300 bg-base-100" bind:this={canvasEl}>
			<div
				class="relative"
				role="group"
				aria-label={t('agents-canvas-label')}
				style="width:{graph.width}px;height:{graph.height}px"
			>
				<svg class="pointer-events-none absolute inset-0" width={graph.width} height={graph.height} aria-hidden="true">
					{#each graph.edges as edge (edge.id)}
						{@const from = byId.get(edge.from)!}
						{@const to = byId.get(edge.to)!}
						{@const hot = edgeOnPath(edge, path)}
						<path d={edgePath(from, to)} class="fill-none {hot ? 'stroke-success stroke-[3]' : 'stroke-base-content/30 stroke-2'}" />
						<circle cx={to.x} cy={to.y + to.h / 2} r="3.5" class={hot ? 'fill-success' : 'fill-base-content/40'} />
					{/each}
				</svg>

				{#each graph.nodes as node (node.id)}
					{@const found = problems.get(node.id) ?? []}
					<button
						type="button"
						data-node={node.id}
						class="card absolute cursor-pointer overflow-hidden border-2 p-2 text-left text-sm transition-shadow motion-reduce:transition-none hover:shadow-md focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary {tone(node)} {found.length ? 'border-error! bg-error/10' : ''} {selectedId === node.id ? 'ring-2 ring-primary' : ''} {onPath(node) ? 'ring-2 ring-success' : ''}"
						style="left:{node.x}px;top:{node.y}px;width:{node.w}px;height:{node.h}px"
						aria-pressed={selectedId === node.id}
						aria-label="{kindLabel(node)}: {title(node)}{found.length ? ` (${t('agents-canvas-issue-count', { count: found.length })})` : ''}"
						onclick={() => void select(node.id)}
					>
						<span class="flex items-center gap-1">
							<span class="truncate text-xs uppercase tracking-wide text-base-content/60">{kindLabel(node)}</span>
							{#if found.length}<span class="badge badge-error badge-xs ml-auto">{found.length}</span>{/if}
						</span>
						<span class="truncate font-semibold {node.kind === 'route' ? 'font-mono' : ''}">{title(node)}</span>

						{#if node.kind === 'main'}
							<span class="truncate font-mono text-xs">{main.model || t('agents-canvas-model-default')}</span>
							<span class="flex flex-wrap gap-1 text-xs">
								<span class="badge badge-ghost badge-sm">{t('agents-canvas-tools', { count: main.tools })}</span>
								<span class="badge badge-ghost badge-sm">{t('agents-canvas-skills', { count: main.skills })}</span>
								<span class="badge badge-ghost badge-sm">{t('agents-canvas-slots', { count: main.slots.length })}</span>
							</span>
							<span class="truncate font-mono text-xs text-base-content/60">{main.slots.join(', ')}</span>
							{#if main.verifiers.length}
								<span class="flex flex-wrap gap-1">
									{#each main.verifiers as v (v.id)}
										<span class="badge badge-outline badge-sm font-mono">{t('agents-canvas-verifier', { id: v.id, kind: v.kind })}</span>
									{/each}
								</span>
							{/if}
						{:else if node.kind === 'gate'}
							<span class="line-clamp-2 break-words font-mono text-xs">{gateText(node.route!) || t('agents-canvas-gate-none')}</span>
							{#if path && node.route! in path.open}
								<span class="badge badge-sm {path.open[node.route!] ? 'badge-success' : 'badge-ghost'}">
									{path.open[node.route!] ? t('agents-canvas-gate-open') : t('agents-canvas-gate-closed')}
								</span>
							{/if}
						{:else if node.kind === 'route'}
							<span class="line-clamp-2 text-xs text-base-content/70">{spec.routes[node.route!]?.description ?? ''}</span>
							{#if path?.picked === node.route}<span class="badge badge-success badge-sm">{t('agents-canvas-picked')}</span>{/if}
						{:else if path && node.route! in path.dispatched}
							<span class="badge badge-success badge-sm">{t('agents-canvas-ran')}</span>
						{/if}
					</button>
				{/each}
			</div>
		</div>

		{#if selected}
			<section class="card card-border min-w-0" aria-labelledby="canvas-panel-title">
				<div class="card-body gap-3 p-4">
					<div class="flex items-start gap-2">
						<div class="min-w-0 flex-1">
							<p class="text-xs uppercase tracking-wide text-base-content/60">{kindLabel(selected)}</p>
							<h3 id="canvas-panel-title" class="truncate font-semibold outline-none" tabindex="-1" bind:this={heading}>
								{selected.kind === 'gate' || selected.kind === 'target' ? `${selected.route} · ${title(selected)}` : title(selected)}
							</h3>
						</div>
						{#if selected.route}
							<button class="btn btn-ghost btn-sm text-error" type="button" onclick={() => remove(selected.route!)}>{t('agents-canvas-remove-route')}</button>
						{/if}
						<button class="btn btn-ghost btn-sm btn-square" type="button" onclick={() => void close()} aria-label={t('agents-canvas-close')}>✕</button>
					</div>

					{#if problems.get(selected.id)?.length}
						<div class="alert alert-error alert-soft text-sm" role="alert">
							<ul class="list-inside list-disc">
								{#each problems.get(selected.id) ?? [] as issue (issue.path + issue.message)}
									<li><span class="font-mono">{issue.path}</span>: {issue.message}</li>
								{/each}
							</ul>
						</div>
					{/if}

					{#key selected.id}
						{#if selected.kind === 'main'}
							<div class="space-y-4">
								<MainAgentForm bind:spec {issues} {granted} {ongrants} />
								<div>
									<h4 class="mb-2 font-semibold">{t('agents-section-state')}</h4>
									<StateSlotsForm bind:spec {issues} />
								</div>
								<div>
									<h4 class="mb-2 font-semibold">{t('agents-router-kind')}</h4>
									<RouterFields bind:spec {issues} models={granted.models} />
								</div>
							</div>
						{:else}
							<RouteEditor
								bind:spec
								name={selected.route!}
								part={selected.kind}
								{issues}
								{agents}
								onrenamed={(name) => void select(`route:${name}`)}
							/>
							{#if selected.kind === 'target' && selected.targetKind === 'agent' && agentOf(spec.routes[selected.route!]?.agent)}
								<a class="link link-primary text-sm" href="{base}/agents/{spec.routes[selected.route!].agent}">{t('agents-canvas-open-agent')}</a>
							{/if}
						{/if}
					{/key}
				</div>
			</section>
		{/if}
	</div>

	{#if !graph.edges.length}
		<p class="text-sm text-base-content/60">{t('agents-canvas-empty')}</p>
	{/if}
	<p class="text-xs text-base-content/60">{t('agents-canvas-keys')}</p>
</div>
