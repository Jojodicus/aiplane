# Diagnose a problem

Start with the failed action, its visible error, and the matching gateway and
upstream logs. Record the build version and the request's time before changing
configuration. Avoid including tokens, cookies, connector secrets or private
document content in a public issue.

| Symptom | Check | Next action |
|---|---|---|
| Process refuses to start | Container logs; session key format; writable data directory | Supply the preserved 64-hex session key and correct mount ownership/path |
| `/readyz` returns `setup_required` | Whether setup completed | Finish the verified OIDC setup flow |
| Provider login fails | Issuer, callback URI, client credentials and provider logs | Correct the actual configured values and retry |
| Administrator access missing | Verified claim values and group mapping | Use host-side `restore-setup`; verify the matching group |
| No models available | Upstreams tab, backend health, pool kind, accessible groups | Test the backend URL from the application network and check its discovered models |
| API returns `401` | Bearer header, token status and expiration | Use an active AIplane token; rotate it if needed |
| API returns model denial | Token model restrictions and pool group access | Select an accessible model or have the administrator correct the intended grant |
| Requests hit a limit | Token budget, user/group/global limits and Usage | Inspect the specific limit and its period; change only the intended policy |
| Tool is absent | Tool registration prerequisites, group grant, user setting and token capability mode | Configure the service and access at their sources |
| File upload fails | Attachment S3 configuration, object-store connectivity, size and type | Correct storage and retry with a supported file |
| Browser control unavailable | Extension installed, configured and enabled for this AIplane | Use Tools → Browser and its status check |
| Connector tools unavailable | Connection state, OAuth/PAT validity, connector ACL and tool grants | Reconnect the account or correct the catalog/grant configuration |
| Knowledge answer incomplete | Collection sync status, indexing errors, grants and search results | Check the actual indexed corpus; sync and retry |
| Voice unavailable | Browser microphone access and configured transcription/speech models | Enable permission and check the relevant model pools |
| Sandbox fails | Runner URL, token, isolation runtime and runner logs | Repair the isolated runner before enabling its tools |
| UI/static manual missing | `AIPLANE_STATIC_DIR` and deployed frontend artifact | Install the full artifact for the application's build |

## Collect logs

```bash
docker compose -f deploy/compose.example.yml logs --tail=200 aiplane
kubectl -n aiplane logs aiplane-0 -c gateway --tail=200
```

Use the command matching your deployment. Connector, OCR and sandbox services
have separate logs. A healthy gateway probe does not prove those services work.

## Report a reproducible problem

Include the build version/commit, installation method, affected page or API
operation, exact steps, expected result, actual result and a redacted error.
Describe relevant grants and optional services. A screenshot of the affected
control is usually clearer than an entire desktop. Report the current behaviour
even when you have a hypothesis about its cause.
