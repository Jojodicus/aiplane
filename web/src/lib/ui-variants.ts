// Class strings for the shared components under components/ui/. They live
// here, outside the .svelte files, so the state → look mapping is one
// testable table and two components can never drift on what "selected" means.

export type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

const TONE: Record<Tone, string> = {
	ok: 'badge-success',
	warn: 'badge-warning',
	bad: 'badge-error',
	info: 'badge-info',
	neutral: 'text-base-content/70'
};

export function statusPillClass(tone: Tone): string {
	return `badge badge-soft rounded-full font-semibold ${TONE[tone]}`;
}

export type StepState = 'current' | 'done' | 'upcoming';

export function stepState(index: number, current: number, completed: readonly number[]): StepState {
	if (index === current) return 'current';
	return completed.includes(index) ? 'done' : 'upcoming';
}

const STEP: Record<StepState, string> = {
	current: 'btn-primary',
	done: 'border-success/40 bg-base-200 text-success',
	upcoming: 'border-base-300 bg-base-200 text-base-content/60'
};

export function stepClass(state: StepState): string {
	return `btn btn-xs rounded-full font-semibold ${STEP[state]}`;
}

export function chipClass(selected: boolean): string {
	return selected
		? 'btn btn-sm rounded-full border-primary bg-primary/15 text-base-content'
		: 'btn btn-sm rounded-full border-base-300 bg-base-200 text-base-content/80';
}

export function choiceCardClass(selected: boolean): string {
	const base = 'flex w-full min-w-0 flex-col items-start gap-1 rounded-box border p-3.5 text-left transition-colors disabled:cursor-not-allowed disabled:opacity-50';
	return selected
		? `${base} border-primary bg-primary/10`
		: `${base} border-base-300 bg-base-200 hover:border-base-content/30`;
}

export function segmentClass(selected: boolean): string {
	return selected ? 'btn join-item btn-primary' : 'btn join-item border-base-300 text-base-content/70';
}
