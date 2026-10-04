# Check for unused translation messages

The Fluent files under `crates/session-core/locales/<language>/` are the source
for user-visible server and browser strings. Every message must have a real
consumer. Run this check after adding or removing a message:

```bash
mise run parity-gap
```

The check compares message IDs from the English catalog with the tracked and
untracked source files. A key without a consumer fails the task. Add the
message where it is used, or remove it when its last consumer is removed. Do
not keep unused translations for a feature that may be added later.

Some IDs are assembled at runtime, for example `nav-group-${name}`. The check
detects dynamic prefixes from source patterns and excludes keys with those
prefixes from the unused list. If a live dynamic message is reported, first
check whether its construction matches one of the patterns recognized in
`web/scripts/parity-gap.py`.

The parity check does not replace `mise run gen-locales` or the locale-completeness
build checks. Fluent catalogs remain canonical; regenerate their TypeScript
consumer after catalog edits and verify the build before committing.
