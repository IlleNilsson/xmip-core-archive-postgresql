# xmip-core-archive-postgresql

PostgreSQL archive target: one item is one row of an archive table. A technology of [xmip-core-archive](https://github.com/IlleNilsson/xmip-core-archive).

The store is the capability's `archive::sql::SqlArchive`; this crate is its
dialect, `PostgreSql` — how the server quotes, how the bytes go in and come back,
how a new row's id is asked for — and the connection through
[xmip-core-transport-postgresql](https://github.com/IlleNilsson/xmip-core-transport-postgresql).
An archive here is `SqlArchive::<PostgreSql>::new(server, database, user)`.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
