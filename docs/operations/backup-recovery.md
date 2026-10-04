# Back up and recover AIplane

A useful backup contains both the persistent state and the keys that decrypt
its secrets. Test restoration in an isolated deployment before relying on it.

## What to preserve

- The AIplane data volume, including SQLite state and the RAG store.
- `AIPLANE_SESSION_KEY` and, when configured, `AIPLANE_ENCRYPTION_KEY`.
- Any separately configured writable directories and attachment object storage.
- Deployment configuration and optional services' persistent stores and secrets.

The default container data directory is `/var/lib/gateway`. An explicit
`AIPLANE_DB_PATH` or a settings path may place state elsewhere. Identify those
actual paths before choosing what to back up. The static UI and documentation
can be restored from the corresponding application build artifact.

## Take a consistent backup

SQLite uses WAL mode. Do not copy only `gateway.sqlite` from a running writer:
the database may also have `-wal` and `-shm` files. For a file-copy backup,
stop the application, back up the complete state directory/volume, and start
it again. Include other stores at a compatible point in time.

For Compose, these commands bracket your volume-backup operation:

```bash
docker compose -f deploy/compose.example.yml stop aiplane
# Back up the aiplane-data volume and any additional state stores.
docker compose -f deploy/compose.example.yml start aiplane
```

For Kubernetes, stop the single writer before a file-copy backup:

```bash
kubectl -n aiplane scale statefulset aiplane --replicas=0
# Wait for the gateway pod to terminate, then back up its persistent volume.
kubectl -n aiplane scale statefulset aiplane --replicas=1
```

Use your storage system's documented procedure. If you use volume snapshots,
verify their consistency guarantees and restoration behaviour rather than
assuming a storage snapshot is an application-consistent backup.

## Restore into an isolated installation

1. Stop the destination application and preserve any state it already holds.
2. Restore the complete data volume and any separate RAG/attachment/service
   stores needed by the backup.
3. Restore the exact session/encryption keys and deployment configuration.
4. Start the application build selected for that backup and check its logs.
5. Verify sign-in, model discovery, a chat, stored connector credentials,
   representative files and knowledge collections.

Do not let the test deployment send scheduled jobs or webhooks to production
systems. Verify the destination's network boundaries and automation state
before bringing it online.

## Recover administrator access

If the provider configuration or administrator mapping is wrong, reopen the
setup wizard from the host while the application continues serving:

```bash
docker compose -f deploy/compose.example.yml exec aiplane restore-setup
```

For Kubernetes:

```bash
kubectl -n aiplane exec aiplane-0 -c gateway -- restore-setup
```

The command prints a one-time link valid for 30 minutes. Open it and complete
a real provider login to repair configuration. Existing users, chats, pools
and the current provider are retained; the command does not log out everyone.
Run it in the application container so it attaches to the correct database.
Protect the printed link as a credential.

Recovery still needs a working identity provider. A bootstrap-admin group
can recover group access for matching verified claims; it does not bypass
identity-provider authentication.
