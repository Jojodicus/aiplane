<!--
SPDX-License-Identifier: AGPL-3.0-only
Copyright (C) 2026 croit GmbH
-->

# Runtime compatibility identifiers

AIplane retains a small set of identifiers so current deployments and independently updated clients continue to work. These are active runtime behaviors, not setup instructions for new installations. New deployments should use the current AIplane names shown in the deployment guides.

## Accepted legacy inputs

The server still accepts selected `GATEWAY_*` environment variables as aliases for `AIPLANE_*`. The `AIPLANE_*` spelling takes precedence; using the old spelling produces a deprecation warning. Set only the current spelling for new deployments. Do not use legacy aliases to override image defaults such as `AIPLANE_DATA_DIR` or `AIPLANE_STATIC_DIR`.

The Helm chart can read an existing session key stored under either `AIPLANE_SESSION_KEY` or `GATEWAY_SESSION_KEY`. It renders the key under both names when managing its Secret. If an externally managed Secret uses the older key name, configure `sessionKey.existingSecretKey` explicitly.

## Published names

Images and the Helm chart are published only under the AIplane names
(`ghcr.io/croit/aiplane*` and `oci://ghcr.io/croit/charts/aiplane`). The former
`ghcr.io/croit/llm-gateway*` images and `llm-gateway` chart receive no further
builds; an installation that still references them stays on its last pulled
version. Point image references at the matching `aiplane` name. To move a Helm
release of the `llm-gateway` chart, upgrade it to `oci://ghcr.io/croit/charts/aiplane`
with `nameOverride: llm-gateway`, which keeps every object name, including the
PersistentVolumeClaim that holds the database.

## Stored-data compatibility

The encryption key derivation still tries retired product-name labels when reading stored secrets. New values are written using the current AIplane label. This allows current releases to read databases created by earlier releases without changing the operator's configured key.

## Stable protocol and storage identifiers

Some old-looking names are part of active contracts and must not be renamed casually:

- `/var/lib/gateway`, `gateway.sqlite`, the `gateway` container user, and the `systemd-gateway` volume name identify existing runtime storage.
- Browser-extension message names and the `gateways` browser storage key are shared between separately updated clients and the server.
- `x-gateway-affinity`, `X-Gateway-Backend`, and `x-gateway-tool-rounds` are client-facing HTTP fields. The affinity header also accepts `x-aiplane-affinity`.
- `[gateway]` configuration keys and the `llmgw-` ComfyUI output prefix remain part of existing configuration and generated-file contracts.
- Sandbox cleanup recognizes the older `app=llm-gateway-sandbox` container label as well as the current label.

These compatibility inputs are implementation details for existing integrations. New configuration and deployments should use the AIplane names documented in the current operator guides.
