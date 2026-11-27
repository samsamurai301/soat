# Backups, restores and upgrades

A SOAT install keeps its state in three places. Back up all three:

| What | Where | Without it |
|---|---|---|
| The database | Postgres (`DATABASE_URL`) | Everything is gone: shops, orders, customers, content |
| Uploaded files | `STORAGE_DIR` (default `./storage`) or the `S3_BUCKET` | Product images and files are gone; their records remain |
| The secrets | `SOAT_SECRET_KEY`, `STORAGE_SECRET` | Keys shops stored (their own Anthropic key, tax and shipping accounts) cannot be opened; file links change |

Keep the secrets in your secret store, not next to the backups: a backup
plus the key is everything an attacker needs.

## Backing up

The database, consistently, while SOAT runs:

```sh
pg_dump --format=custom --file=soat-$(date +%F-%H%M).dump "$DATABASE_URL"
```

A custom-format dump is compressed and restores selectively. Run it at
least daily, keep several days (and some weeks), and copy it off the
machine. For point-in-time recovery, use your provider's continuous
backups or WAL archiving (pgBackRest, WAL-G) instead.

Files on disk: copy `STORAGE_DIR` after the dump (`rsync -a`, restic,
borg). Files are written before the rows that name them, so a copy taken
after the dump holds every file the dump refers to. On S3, turn on bucket
versioning or replication.

## Restoring

1. Stop SOAT.
2. Restore into an empty database, owned by the role SOAT connects as:

   ```sh
   createdb --owner=soat soat
   pg_restore --no-owner --role=soat --dbname=soat soat-2026-11-27-0300.dump
   ```

3. Put the files back where `STORAGE_DIR` points (or restore the bucket's
   versions).
4. Start the same SOAT version that made the dump, or a newer one, with the
   same `SOAT_SECRET_KEY` and `STORAGE_SECRET`.

Row-level security comes back with the dump: the policies and
`FORCE ROW LEVEL SECURITY` are part of the schema. Connect SOAT as a role
that owns the tables but is not a superuser, because superusers skip
row-level security.

Try a restore now and then, on another machine: a backup nobody restored is
a guess.

## Upgrading

1. Take a backup (above).
2. Stop the old version and start the new one. At startup it applies its
   new migrations, each in a transaction, before it serves a request. To
   migrate as a separate step (a release job), run the new binary once with
   `ZAPLINE_MIGRATE=up`: it migrates and exits. `ZAPLINE_MIGRATE=status`
   lists every migration and whether it is applied.
3. Several SOAT processes on one database: stop them all, upgrade one, then
   start the rest on the new version.

**An older SOAT refuses a newer database.** When the database has a
migration the binary does not know, because a newer version or a build
with more features migrated it, startup stops and names those migrations.
That way an old binary never misreads tables it does not understand. To go
back a version, restore the backup from before the upgrade. Every
migration also has a down migration: with the *new* binary,
`ZAPLINE_MIGRATE=down:<n>` reverts the last `n`, but data written since
the upgrade may not survive the way back, so prefer the backup.
