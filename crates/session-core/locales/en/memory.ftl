# Strings owned by `gateway/src/rama_server/pages/memory.rs` — the
# per-user /memory page: inspect, add, edit, and delete the structured
# memories the assistant keeps about you.

memory-heading = Memory
memory-description = What the assistant remembers about you, grouped by kind. Add, edit, or delete entries here — it's your account's memory and fully under your control. Turn the capability on or off on the Tools page.

memory-add-heading = Add a memory
memory-kind-aria = Memory kind
memory-content-placeholder = e.g. Prefers answers in metric units

memory-empty = Nothing here yet.
memory-save-button = Save
memory-delete-title = Delete memory

# SPA-only: the Svelte /memory page's inline add/edit form.
memory-kind-preference = Preferences
memory-kind-project = Project context
memory-kind-fact = Facts
memory-content-label = Content
memory-add-button = Remember
memory-delete-confirm = Delete this memory?

# The per-category Add button in each card header; it opens the add dialog
# with that card's kind already selected.
memory-add-short = Add

# One line under each card heading saying how that kind reaches the
# assistant: preferences ride in the system context of every conversation
# while memory is on, within the limits the server reports ($count, $chars),
# while project notes and facts wait to be looked up with `recall`. The
# difference changes which bucket a user files something in, and nothing
# else on the page reveals it.
memory-kind-preference-hint = Sent with every conversation while memory is on, so the assistant applies them without being asked. Newest first, up to { $count } preferences and about { $chars } characters; the newest one is always included.
memory-kind-project-hint = Fetched on demand — the assistant looks these up when the conversation touches your work.
memory-kind-fact-hint = Fetched on demand — the assistant looks these up when they become relevant.

# Shown on the Preferences card when memory is off or not granted, so the
# hint above does not promise what the server will not do.
memory-preference-not-in-context = Memory is switched off or not granted to you, so these preferences are not sent to the assistant. Switch memory on under Tools, or ask an administrator for access.
