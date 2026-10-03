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

1. In the agent builder's **Sharing** tab, create an **embed key** for the
   agent and list the exact origins that may use it (`https://www.example.com`, scheme and host,
   plus the port if it is not the default). The key is shown once, already
   inside the snippet below.
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
| `data-identity-token` | a token your site signed for the signed-in visitor ([below](#signed-in-visitors)) | none |

## Verification codes

When the agent verifies a visitor's email address (an `mcp_code` verifier),
the panel shows a dedicated field under the conversation: masked, offered for
the browser's one-time-code autofill, and sent straight to the verifier. The
code never becomes a chat message, so neither the assistant nor the
conversation's transcript ever contains it. Cancel declines the request.

## Signed-in visitors

If your site already knows who the visitor is, it can say so with a short-lived
JSON Web Token it signs, and the agent's `host_jwt` verifier fills its slots
from the token's claims — no code to type. Configure the verifier in the
agent's spec ([`agents.md`](agents.md#what-95-built)): the algorithm (HS256
with a shared secret, or RS256/ES256 with your public key or a JWKS address),
the `issuer` and `audience` your tokens carry, and which claim fills which
slot.

Sign on your server, never in the browser:

```json
{ "iss": "https://www.example.com", "aud": "support-agent", "sub": "K-12345",
  "iat": 1790000000, "exp": 1790000300, "jti": "a-fresh-random-id" }
```

- `exp`, `iat`, `iss` and `aud` are required; the token may live at most the
  verifier's `max_lifetime` (10 minutes by default). A `jti` makes it usable
  once.
- Pass it in the snippet, or later — after your own login — from script:

```html
<script src="https://YOUR-GATEWAY/embed.js" data-agent-key="gwe_..."
        data-identity-token="eyJhbGciOi..." async></script>
```

```js
document.querySelector('croit-aiplane-embed').setIdentityToken(freshToken);
```

The widget sends it once per conversation to `POST /api/v0/embed/identity`.
A refused token leaves the conversation working without it and logs the
reason to the browser console (`identity_token_invalid: it has expired`, …).

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
- An identity token you pass is held in memory only and sent to the gateway
  once per conversation; it is never stored by the widget.
- Answers are shown as text. A small markdown subset (paragraphs, lists, code,
  bold, emphasis, `http(s)` links) is built with DOM calls, never as HTML.

## When the agent asks a person

An agent can hand a conversation to your staff (a `human` route) or wait for
their approval before a tool runs (`permission: always_ask`). The visitor
then sees a notice that a member of staff will answer, and the widget checks
for the answer every 10 seconds. The visitor can keep typing: a message sent
meanwhile waits and is answered after the staff member's.

Your staff answer in AIplane's inbox (`/inbox`). Whoever holds a `write`
share on the agent, and the users and groups listed as the agent's
**responders**, see the item; responders see only the item, not the agent's
settings or its other conversations. They can be told by Web Push and by
Slack or Discord incoming webhooks you add under the agent's Sharing tab.
Those messages carry the agent's name, what kind of request it is and a link
to the inbox; they carry the question only if you turn on details for the
channel, and never what the visitor wrote.

If nobody answers in time (30 minutes unless the route sets `timeout`), the
visitor is told so in their language and the conversation goes on. See
[`agents.md`](agents.md#what-96-built) for the spec keys.

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
