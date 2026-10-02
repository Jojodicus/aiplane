# Embedding an agent on your website

A published agent can be put on any website with one script tag. This page is
for the person who owns that website. The design behind it (visitor sessions,
why a token in `sessionStorage` and not cookies) is
[`agents.md` §5](agents.md#5-visitor-sessions-and-embedding).

## The snippet

```html
<script
  src="https://YOUR-GATEWAY/embed.js"
  data-agent-key="gwe_..."
  async></script>
```

1. In the agent builder, create an **embed key** for the agent and list the
   exact origins that may use it (`https://www.example.com`, scheme and host,
   plus the port if it is not the default). The key is shown once.
2. Paste the snippet before `</body>` on those pages.

A launcher button appears in the bottom corner. The visitor's conversation
starts when they send their first message.

## Attributes

| Attribute | Values | Default |
|---|---|---|
| `data-agent-key` | the `gwe_…` key (required) | |
| `data-lang` | `en` `de` `fr` `es` `ru` `zh` | the browser's language, then `en` |
| `data-theme` | `light` `dark` | follows the visitor's OS setting |
| `data-position` | `left` `right` | `right` |
| `data-title` | panel title | the agent's display name |

## Styling

The widget lives in a shadow root, so your page's CSS does not reach it and
its CSS does not leak out. It is built from daisyUI components, so it is
themed with daisyUI's custom properties, set on the `croit-aiplane-embed`
element in your own stylesheet:

```css
croit-aiplane-embed {
  --color-primary: #0b6bcb;          /* header, launcher, your bubbles */
  --color-primary-content: #ffffff;  /* text on the primary color */
  --color-base-100: #ffffff;         /* panel background */
  --color-base-200: #f6f6f6;         /* code blocks */
  --color-base-300: #e7e3ec;         /* borders, assistant bubbles */
  --color-base-content: #1d1d1b;     /* text */
  --color-error: #b3261e;
  --radius-box: 0.75rem;             /* panel and launcher corners */
  --radius-field: 0.5rem;            /* input and code blocks */
}
```

The widget stays out of the way of the page: it adds one element to `<body>`,
uses the system font stack (no web fonts are loaded) and its animations stop
for visitors who ask for reduced motion.

## What the widget holds, and what the key is

- The only values in the widget are the **embed key** and a **visitor token**
  (`gwv_…`). The embed key is public by design: it is in your page source.
- The visitor token lives in the tab's `sessionStorage` (a reload keeps the
  conversation, closing the tab ends it) and is sent as an `Authorization`
  header. No cookies are set or read.
- The origin list on the key stops *other websites* from embedding your agent.
  It does not stop a script that fakes an `Origin` header. Abuse is limited by
  the agent's grants and gates, and by rate limits and the owner's budget.
- Answers are shown as text. A small markdown subset (paragraphs, lists, code,
  bold, emphasis, `http(s)` links) is built with DOM calls, never as HTML.

## Content Security Policy

If your site sends a CSP, allow the gateway for two things:

```
script-src  https://YOUR-GATEWAY;
connect-src https://YOUR-GATEWAY;
```

Nothing else is needed: the styles are a constructed stylesheet (not subject to
`style-src`), there are no images, fonts or frames, and no inline script.

## Try it locally

`mise run dev-ui` seeds a published agent and an embed key for
`http://localhost:8000`. Serve the example page from that origin:

```bash
python3 -m http.server 8000 -d examples/embed
```

Open `http://localhost:8000/`. The page ([`examples/embed/index.html`](../examples/embed/index.html))
deliberately styles buttons and paragraphs in red Comic Sans to show that the
widget is unaffected.

## For maintainers

- Source: `web/embed/` (plain TypeScript, no framework), built by
  `mise run build-web` to `target/frontend/build/embed.js` with
  `web/vite.embed.config.ts`. It shares nothing with the SPA bundle.
- The gateway serves it at `/embed.js` like any file in `AIPLANE_STATIC_DIR`,
  with a 5-minute cache so a fixed widget reaches sites quickly.
- Strings are the `embed-*` keys of the Fluent catalogs; `mise run gen-locales`
  also writes `web/embed/locales.generated.ts` (all six languages, `embed-*`
  only).
- `forShadowRoot` rewrites daisyUI's `:root` theme selectors to `:host` and
  turns Tailwind's `@property` defaults into plain declarations, because
  neither works inside a shadow root as shipped.
