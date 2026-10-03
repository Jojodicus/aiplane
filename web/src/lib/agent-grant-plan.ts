/**
 * Grant changes the setup stages instead of making at once (#116). A card
 * switched on, a model chosen or an identity system picked needs a grant on
 * the agent's principal; doing that on the click would leave the grant behind
 * when the person then cancels the edit. So the steps stage their changes
 * here, they show the grants as they would be, and the workspace carries the
 * plan out on save — grants before the draft (the validator checks them),
 * revocations after it (the saved draft no longer uses them) — or drops it on
 * Cancel.
 */
import type { Grant, GrantKind, Spec } from './agents.ts';
import { liveUses } from './agent-setup.ts';

export interface GrantRef {
	kind: GrantKind;
	ref: string;
}

export interface GrantPlan {
	grant: GrantRef[];
	revoke: GrantRef[];
}

export const emptyPlan = (): GrantPlan => ({ grant: [], revoke: [] });

const same = (a: GrantRef, b: GrantRef) => a.kind === b.kind && a.ref === b.ref;
const held = (grants: Grant[], r: GrantRef) => grants.some((g) => same(g, r));

export function isEmpty(plan: GrantPlan): boolean {
	return !plan.grant.length && !plan.revoke.length;
}

/** `plan` with `r` to be granted: nothing to do when the agent holds it, and a staged revocation of it is withdrawn. */
export function stageGrant(plan: GrantPlan, grants: Grant[], r: GrantRef): GrantPlan {
	const revoke = plan.revoke.filter((x) => !same(x, r));
	const grant = held(grants, r) || plan.grant.some((x) => same(x, r)) ? plan.grant : [...plan.grant, r];
	return { grant, revoke };
}

/**
 * `plan` with `r` given up. A staged grant of it is simply withdrawn; a grant
 * the agent holds is revoked unless the published version still uses it,
 * which `kept` reports.
 */
export function stageRevoke(plan: GrantPlan, grants: Grant[], live: Spec | null, r: GrantRef): { plan: GrantPlan; kept: boolean } {
	if (plan.grant.some((x) => same(x, r))) return { plan: { ...plan, grant: plan.grant.filter((x) => !same(x, r)) }, kept: false };
	if (!held(grants, r)) return { plan, kept: false };
	if (liveUses(live, r.kind, r.ref)) return { plan, kept: true };
	return { plan: plan.revoke.some((x) => same(x, r)) ? plan : { ...plan, revoke: [...plan.revoke, r] }, kept: false };
}

/** The grants as they will be once the plan is carried out, for the steps to show. */
export function plannedGrants(grants: Grant[], plan: GrantPlan): Grant[] {
	const kept = grants.filter((g) => !plan.revoke.some((r) => same(r, g)));
	return [...kept, ...plan.grant.map((r) => ({ ...r, granted_by: '', granted_at: '' }))];
}
