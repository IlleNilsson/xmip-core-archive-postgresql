# xmip-core-archive-postgresql

PostgreSQL archive target: one item is one row of an archive table. A technology of [xmip-core-archive](https://github.com/IlleNilsson/xmip-core-archive).

The store is the capability's `archive::sql::SqlArchive`; this crate is its
`archive::sql::Database`, `PostgreSql` — the SQL the server speaks (`PostgreSQL`'s, `sql::POSTGRESQL`,
from `xmip-core-library-sql`) and the connection through
[xmip-core-library-postgresql](https://github.com/IlleNilsson/xmip-core-library-postgresql),
the protocol the PostgreSQL transport speaks too; until 2026-11-13 it was
reached through that transport.
An archive here is `SqlArchive::<PostgreSql>::new(server, database, user)`.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
