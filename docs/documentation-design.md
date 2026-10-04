# Documentation design

The documentation describes the current product from the same source revision
as the application. It serves people evaluating AIplane, people using it,
administrators, operators, API clients, and language models.

## Shared mechanisms

- Existing Markdown is investigated against its implementation before reuse.
- `docs/tools-inventory.md` and its drift test supply the tool inventory.
- Registered Rama routes and wire types supply the API inventory.
- Svelte routes and their actions supply the user-interface inventory.
- The existing settings registry supplies the configuration inventory.
- `mise run version` and Git supply build identity.
- `build-web`, `AIPLANE_STATIC_DIR`, the SPA file resolver and the container's
  existing frontend artifact supply local delivery.
- `dev-ui` supplies isolated, synthetic screenshot data. Screenshots capture
  actual relevant elements without exposing the maintainer's data.

## Content and navigation

The README introduces verified benefits, shows the product, and provides a
deployment-current quickstart. The manual groups getting started, user tasks,
administration, agents, operations, and reference. Each task explains its
purpose, prerequisites and permissions, steps, observable result, limitations,
and diagnosis. Source references identify the implementation reviewed.

MkDocs navigation explicitly selects the pages published. Unreviewed design
records are not silently included in the manual, search, or LLM exports.

## Delivery

The same Markdown builds a static website for GitHub Pages and for `/docs/`
inside the existing frontend artifact. Navigation, search and assets work
without external services. Build identity is visible. Markdown sources,
`llms.txt`, and `llms-full.txt` are generated from the published content.
There is no application database change or compatibility work for old installs.

## Verification

Coverage inventories enumerate UI pages, registered API operations, settings
and tools. Inventory presence and factual/practical review are distinct checks.
Local links, build output and LLM exports are checked; the bundled manual is
opened in a browser, including deep links and search. Serving tests cover
directory indexes, missing files, caching and traversal. Product deficiencies
are tracked as GitHub issues; the manual describes current behaviour.
