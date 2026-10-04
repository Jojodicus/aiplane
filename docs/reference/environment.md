# Environment and process reference

The environment configures where the process runs and how it finds its state.
Model connections, groups, identity-provider configuration and operator
settings are stored in the database and edited through the application.

## Application runtime

| Variable | Default or required value | Purpose |
|---|---|---|
| `AIPLANE_SESSION_KEY` | Required: 64 hexadecimal characters | Browser session signing; also the default source for at-rest encryption key derivation. Preserve for the deployment's lifetime. |
| `AIPLANE_ENCRYPTION_KEY` | Optional: 64 hexadecimal characters | Separate AES-256-GCM key for stored secrets. When absent, derive from the session key. Preserve with backups when configured. |
| `AIPLANE_DATA_DIR` | Process working directory when unset; container `/var/lib/gateway` | Base writable state directory used for default database and subsystem paths. |
| `AIPLANE_DB_PATH` | `gateway.sqlite` under the data directory | Explicit SQLite database path. |
| `AIPLANE_STATIC_DIR` | Unset; container `/usr/share/gateway/ui` | Installed SPA artifact, including its `docs/` subdirectory. Missing frontend files produce a static-site deployment error. |
| `IP` | `127.0.0.1`; container `0.0.0.0` | Listen IP. Invalid values are warned about and fall back to the default. |
| `PORT` | `8080` | Listen port. Invalid values are warned about and fall back to the default. |
| `AIPLANE_PUBLIC_URL` | No environment override | Public URL fallback before setup stores the configured URL. Useful behind a reverse proxy. |
| `AIPLANE_BOOTSTRAP_ADMIN_GROUPS` | Empty | Comma-separated verified claim values that grant bootstrap administrator access. Does not replace OIDC authentication. |
| `AIPLANE_TRUSTED_PROXIES` | Empty | Comma-separated proxy IPs/CIDRs allowed to supply forwarded client IPs. Invalid entries fail configuration. |
| `AIPLANE_ALLOW_PRIVATE_NETWORKS` | False | Outbound network policy for URLs selected by users/models/agent owners. See the policy details below. |
| `AIPLANE_SOURCE_URL` | `https://github.com/croit/aiplane` | Repository linked by the application's Source link; set to the source of a custom deployment. |
| `AIPLANE_VERSION` | Cargo package version if unset/blank | Runtime version label. Official container builds supply the release version. |
| `RUST_LOG` | Set by the deployment/task | Tracing filter. Keep sensitive request content out of shared logs. |
| `PDFIUM_LIB_PATH` | Container `/usr/local/lib/libpdfium.so` | PDFium library used by the PDF reader's image rendering. |

The private-network switch accepts `1`, `true`, `yes`, `on` and the corresponding
false values `0`, `false`, `no`, `off` (case-insensitive); an empty value is
false. The outbound guard still rejects metadata/link-local destinations.
It is a deployment-level policy, not an administrator UI switch. Inspect
the network policy implementation for address-class details.

Operator-configured upstream backends and user-selected URLs have different
HTTP clients. Environment proxies do not apply to guarded user/model-selected
destinations: those use the checked, pinned address directly.

## Command-line interface

| Invocation | Result |
|---|---|
| `aiplane` | Start the server using its environment and stored settings. |
| `aiplane --help` or `aiplane -h` | Print command-line usage. |
| `aiplane restore-setup` | Attach to the deployment database and print a one-time 30-minute setup recovery link. |

Unknown arguments fail with usage information. The runtime container provides
`restore-setup` as a convenience executable for the recovery command.

## Development and documentation

| Setting/task | Purpose |
|---|---|
| `DEV_UI_BIND` | Address of the synthetic `dev-ui` fixture; defaults to `127.0.0.1:8080`. Use a separate port alongside a developer's running server. |
| `AIPLANE_DEV_PUBLIC_PORT` | Vite's public port in the developer workflow. |
| `AIPLANE_DEV_BACKEND_ORIGIN` | Backend origin used by Vite's gateway proxy. |
| `mise run build-web` | Build/stage the SPA and current manual together. |
| `mise run build-docs` | Build the manual and Markdown/LLM exports. |
| `mise run dev-docs` | Serve a local documentation preview. |
| `mise run docs-screenshots` | Capture documented UI elements from the isolated synthetic fixture. |

These are development/build settings and do not add runtime configuration
fields to an installation. See the [documentation system](../documentation-system.md)
for build identity and exports.

## Optional services

OCR adapter, sandbox runner, ComfyUI and MCP services are separate processes
with their own environment contracts. Configure their actual deployed URLs
and credentials in the relevant AIplane settings/catalog. See
[deployment](../operations/deployment.md) and
[operator settings](../admin/settings.md); do not assume an environment
variable intended for a sidecar configures the gateway as well.
