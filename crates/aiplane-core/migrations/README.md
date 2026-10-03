# Migrations

`sqlx::migrate!()` runs every file in this directory in numeric
order at gateway startup. The first run inserts a row per
file into the `_sqlx_migrations` table, recording the migration
number, a checksum of the file bytes, and the timestamp it was
applied.

## Treat applied migrations as immutable

On every subsequent startup sqlx re-hashes each migration file
and compares it to the recorded checksum. **Any change — including
whitespace and comment edits — bumps the hash and the gateway
refuses to boot:**

```
Error: running migrations
Caused by: migration N was previously applied but has been modified
```

This is by design: the recorded checksum is sqlx's way of catching
"someone hand-edited migration history out from under the
production DB."

### Rules

1. **Never edit a `.sql` file that has been committed to `main`.**
   That includes comments. Once it's merged, assume it's running
   in production and is locked.
2. **Need to change something?** Add a new numbered migration
   (`000N_describe_change.sql`) with the schema delta. SQLite can
   do most things with `ALTER TABLE`; the rest goes through the
   `CREATE TABLE new` → `INSERT ... SELECT` → `DROP TABLE old` →
   `ALTER TABLE new RENAME` dance.
3. **Need to update a comment?** Put the prose in the migration's
   *commit message*, or in this README — not in the `.sql`.
4. **Not pushed yet?** A migration that is not on `origin/main` has
   run on no installation, so it is amended rather than followed by a
   new one: all unpushed work shares one migration (today
   `0077_agent_builder.sql`, which squashed the agent-builder chain and
   took the agent architect's tables in #118). Amending it means, in
   the same commit: replace its line in
   `tests/migration-checksums.txt` with the one
   `no_released_migration_has_been_modified` prints, regenerate
   `tests/fixtures/schema_after_agent_builder.txt` with
   `UPDATE_SCHEMA_FIXTURE=1 mise run test-crate aiplane-core migration_0077`
   and review its diff, and keep `tests/migration_0077.rs` passing (the
   lossless upgrade from the last pushed migration). Developers whose
   local database already ran the old bytes must recreate it. The
   moment the migration is pushed, rule (1) applies.

### Migrations run with foreign keys off

`db::open` applies migrations on one connection with
`PRAGMA foreign_keys = OFF`, then runs `PRAGMA foreign_key_check`
and refuses to boot if any row points at nothing. This is SQLite's
documented procedure for a table rebuild, and it is the only way to
get it: sqlx wraps every migration in a transaction, and inside one
the pragma is a no-op.

Why it matters: rebuilding a table that others reference
(`chat_sessions`, in migration `0077_agent_builder.sql`) means
`DROP TABLE` on the old one. With foreign keys on, that drop first deletes every row, and every
`ON DELETE CASCADE` child goes with it — silently, inside a migration
that otherwise succeeds. With them off, the children keep naming the
table, and after the rename that name is the new table again.

Consequences for writing a migration:

- A `DELETE` inside a migration does **not** cascade. Delete child
  rows explicitly, or the post-migration check fails the boot.
- Rebuild a parent in place (`CREATE … _new`, copy, `DROP`, `RENAME`);
  the children need no rebuild.
- A rebuild that loses rows is the failure the check cannot see.
  Write a test that migrates a populated file database from the
  previous release (`tests/migration_0077.rs` is the template).

### What enforces this

Rules (1) and (3) used to live only in this file, and prose is not
something a tree-wide `sed` reads. The rename to croit AIplane
rewrote a documentation path in a comment inside `0013_rag.sql` —
a migration from the initial public release — and every existing
installation stopped booting. `0013_rag.sql` still points at
`docs/rag.md`, a file that no longer exists, and that stale
pointer stays: correcting it is exactly the edit that caused the
outage.

`crates/aiplane-core/tests/migrations_are_frozen.rs` now pins
every migration's sqlx checksum in
`crates/aiplane-core/tests/migration-checksums.txt` and compares
on every test run, so the same mistake fails in CI rather than at
an operator's next restart. A new migration appends one line —
the test prints it. Nothing else in that file is ever edited.

The one exception is a migration that has never been pushed: no
database outside a developer's machine has it, so a run of them may
be squashed into one, as `0077_agent_builder.sql` squashed 22
unpushed migrations. Their lines go with them. The squash has to
prove it is the same schema (`tests/migration_0077.rs` compares a
fresh database with a dump of the chain it replaced, in
`tests/fixtures/`) and the same lossless upgrade from the last
pushed migration.

### Recovering from an accidental edit

If the boot is already broken by a checksum mismatch and you
*haven't* changed the SQL itself (only comments / formatting),
the quickest path is to restore the file to its pre-edit content
via `git show <prior-commit>:crates/aiplane/migrations/000N_….sql`
and commit that. The DB never needed migrating; only the file
had to match what was applied.

If the SQL *did* change, do not retroactively rewrite history —
instead, add a follow-up migration that brings the schema to the
desired state. Operators running the previous version need the
file to keep matching their `_sqlx_migrations` row.
