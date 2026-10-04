# Manage access, tokens and limits

Use **Administration → Access** to manage groups, inspect users and API tokens, and define usage limits. These administration endpoints require the `admin` role. User creation and identity authentication come from OIDC; the Users screen lists identities that AIplane knows.

## Map identity groups to AIplane permissions

Open `/admin/groups`. The page has **Groups**, **Identity**, **Tools** and **Skills** tabs.

1. Inspect **Identity** to see observed OIDC values and their current mappings.
2. Create a group from a value, or add a group in **Groups**.
3. Configure the group's name, description, OIDC values, administrator/default flags, tools and skills.
4. Save and verify the mapping in **Identity** and the effective roles in **Users**.
5. Use the **Tools** and **Skills** matrices to compare or edit grants across groups.

The matrices edit the same group grants as the group form. Tool rows include individual tools and discovered families such as an MCP connector or ComfyUI. A wildcard grants a wider set than individual entries; inspect the resulting matrix before using it. Deleting a group requires confirmation and changes the permissions resolved through that group.

`can_manage_agents` is the permission for the agent builder; administrator access implies it. The Groups form does not expose this flag. For a non-administrator builder group, use the session-authenticated administration API: read `GET /api/v0/admin/groups`, take the group's complete object, set `can_manage_agents` to `true`, and submit it with `PUT /api/v0/admin/groups`. Preserve its OIDC/tool/skill lists and administrator/default flags: the endpoint replaces those values. Omitting `can_manage_agents` leaves that particular flag unchanged. Verify the returned group afterwards.

Builder permission does not itself give an agent permission to use every model or tool: the agent has separate grants, documented in [agent permissions](../agent-guide/permissions.md).

Pool allowed groups, collection allowed groups, connector allowed groups, tool grants and skill grants serve different resources. A tool being listed in a permission matrix does not establish that its external service is configured or reachable.

## Inspect users and impersonation

Open `/admin/users` to see user identity, observed OIDC groups, resolved gateway roles and join time. Use these values to diagnose a missing permission rather than guessing which identity-provider claim was received.

If `gateway.allow_impersonation` is enabled, another user's row offers an impersonation action with confirmation. The session changes to that user's view. Use the impersonation controls in the application to return to the administrator session. The Users page shows the impersonation audit with timestamp, action, administrator and target. If the setting is disabled, the action is unavailable.

Impersonation exposes the selected user's application context. Configure this setting deliberately in [system settings](settings.md); the documentation does not change its enabled state.

## Restrict API tokens to models

Open `/admin/tokens` to inspect all tokens, their owner, state, dates, request/token counts, cost and model scope. Personal token creation is in the user's Settings → Tokens screen.

1. Find the token by its name and owner.
2. Edit its model restriction in the token row.
3. Enable restriction and select the allowed model names, or disable restriction to remove that token-specific list.
4. Save and inspect the updated scope.

A token restriction narrows model access. Removing it does not grant rights beyond the owner's existing access. Usage columns depend on usage accounting. The list does not recover plaintext bearer secrets.

## Set request, token or cost limits

Open `/admin/limits`. Ensure `limits.enabled` is on in [system settings](settings.md).

1. Choose the subject: global, group/role, user, token or system principal/agent.
2. Select the concrete subject when the scope is not global.
3. Choose all models or a particular model.
4. Choose requests, tokens or cost, then hour, day, week or month.
5. Enter the limit and save.
6. Check the rule table and the user's limit display on **Usage**. Use Edit to change a rule or Delete with confirmation to remove it.

Costs use the configured currency and model pricing. Missing prices make a cost-based policy incomplete; verify the unpriced-model warning before using monetary totals to judge consumption. A pool's `enforce_limits` declaration also affects enforcement and belongs in the routing review.

## Read usage correctly

Open `/usage`. Available time presets are today, last 24 hours, this week, last week, this month and last month. Filter by source (API, chat, scheduled actions or agents), backend and API token. Administrators can view all users or their own scope; the server enforces access to all-user data.

The page shows summary metrics and breakdowns by backend, source, model and token, plus a user breakdown for the all-user scope. Self scope can display applicable limits. An unpriced-model warning means cost totals do not cover every model's usage. Disabled accounting is explicitly shown; it is not a zero-use result. Retention controls how far recorded history extends.

## Troubleshooting

| Symptom | Check |
|---|---|
| Admin page returns forbidden | Effective `admin` role shown for the signed-in identity |
| Group does not match | Actual observed OIDC values, group mapping and effective roles |
| Builder unavailable | `can_manage_agents` on a resolved group |
| Token cannot request a model | Token scope and owner access to the model's pool |
| Limit has no effect | `limits.enabled`, subject/model match and pool `enforce_limits` |
| Cost looks too low | Unpriced models, configured prices, filters and retention |
| Impersonation action absent | `gateway.allow_impersonation`; own row cannot impersonate itself |
