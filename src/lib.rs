#![forbid(unsafe_code)]

//! `PostgreSQL` archive: an [`ArchiveStore`] that keeps each retained item as
//! one row of an archive table, and restores it by selecting the row back.
//!
//! A xmip-core-archive **technology** (repository-model.md): it depends on
//! the archive capability for the [`ArchiveStore`] trait and its item,
//! receipt and error types, and on the `PostgreSQL` transport technology for
//! the connection — the simple query flow, trust or a cleartext password.
//! One item is one row with the four columns every archive technology
//! carries — `data_type`, `identifier`, `bytes`, `metadata` — and
//! `archived_at`, when it was handed over. The table is the operator's to
//! create; this is the shape it is written for:
//!
//! ```sql
//! CREATE TABLE archive (
//!     id          bigserial PRIMARY KEY,
//!     data_type   text NOT NULL,
//!     identifier  text NOT NULL,
//!     bytes       bytea NOT NULL,
//!     metadata    text NOT NULL,
//!     archived_at timestamptz NOT NULL
//! );
//! ```
//!
//! An archive never deletes (ADR-0040): this one inserts and selects, nothing
//! else. The receipt is `postgresql://<server>/<database>/<table>?id=<n>`,
//! and restoring reads the table and the id from it on the store's own
//! connection. The metadata text, the timestamp, the shared row code and
//! the receipt parser come from the archive capability (ADR-0044); the
//! row's dialect is this crate's.

pub mod row;

use std::time::Duration;

use archive::{ArchiveError, ArchiveItem, ArchiveReceipt, ArchiveStore, location, timestamp};
use postgresql::Client;

/// The table written to unless told otherwise.
pub const DEFAULT_TABLE: &str = "archive";

/// An archive that keeps items as rows of one table on one server.
pub struct PostgresqlArchive {
    server: String,
    database: String,
    user: String,
    password: Option<String>,
    table: String,
    timeout: Option<Duration>,
}

impl PostgresqlArchive {
    /// An archive writing to [`DEFAULT_TABLE`] in `database` at `server`,
    /// logging in as `user` by trust.
    #[must_use]
    pub fn new(
        server: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
    ) -> Self {
        Self {
            server: server.into(),
            database: database.into(),
            user: user.into(),
            password: None,
            table: DEFAULT_TABLE.to_string(),
            timeout: None,
        }
    }

    /// The password to give when the server asks for one in the clear.
    #[must_use]
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// The table to write to, `audit.archive` say.
    #[must_use]
    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = table.into();
        self
    }

    /// Give up on a server that stops mid-message.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn connect(&self) -> Result<Client, ArchiveError> {
        Client::connect(
            &self.server,
            &self.user,
            &self.database,
            self.password.as_deref(),
            self.timeout,
        )
        .map_err(ArchiveError::caused_by)
    }

    fn location(&self, id: &str) -> String {
        format!(
            "postgresql://{}/{}/{}?id={id}",
            self.server, self.database, self.table
        )
    }
}

impl ArchiveStore for PostgresqlArchive {
    fn archive(&self, item: ArchiveItem) -> Result<ArchiveReceipt, ArchiveError> {
        let sql = row::insert_sql(&self.table, &item, &timestamp::now());
        let mut client = self.connect()?;
        let result = client.query(&sql).map_err(ArchiveError::caused_by)?;
        client.close().map_err(ArchiveError::caused_by)?;
        let id = result
            .rows
            .first()
            .and_then(|first| first.first())
            .cloned()
            .flatten()
            .ok_or_else(|| ArchiveError {
                message: format!("the insert into {} returned no id", self.table),
            })?;
        Ok(ArchiveReceipt {
            location: self.location(&id),
            checksum: None,
        })
    }

    fn restore(&self, receipt: &ArchiveReceipt) -> Result<ArchiveItem, ArchiveError> {
        let (table, id) = location::table_row("postgresql", &receipt.location)?;
        let mut client = self.connect()?;
        let result = client
            .query(&row::DIALECT.select_sql(table, id))
            .map_err(ArchiveError::caused_by)?;
        client.close().map_err(ArchiveError::caused_by)?;
        let first = result.rows.first().ok_or_else(|| ArchiveError {
            message: format!("no row at {}", receipt.location),
        })?;
        row::DIALECT.item_from_row(first, &receipt.location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archive::fixture::{item, secs};
    use postgresql::bytea;
    use postgresql::{Answer, Event, Session};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// A far end that serves `connections` clients in turn: any INSERT is
    /// answered with id 41, any SELECT with the canned row for `held`, and
    /// every statement is reported back.
    fn far_end(
        password: Option<&'static str>,
        held: &ArchiveItem,
        connections: usize,
    ) -> (String, JoinHandle<Vec<Event>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address").to_string();
        let row = [
            Some(held.data_type.clone()),
            Some(held.identifier.clone()),
            Some(bytea::hex_literal(&held.bytes)),
            Some(archive::metadata::encode(&held.metadata)),
        ];
        let handle = std::thread::spawn(move || {
            let mut events = Vec::new();
            for _ in 0..connections {
                let Ok(session) = Session::accept(&listener, password, Some(secs(2))) else {
                    continue;
                };
                let canned = [
                    row[0].as_deref(),
                    row[1].as_deref(),
                    row[2].as_deref(),
                    row[3].as_deref(),
                ];
                let rows: [&[Option<&str>]; 1] = [&canned];
                let mut session = session
                    .with_table(&["data_type", "identifier", "bytes", "metadata"], &rows)
                    .answering(|sql| {
                        sql.starts_with("INSERT").then(|| Answer::Rows {
                            columns: vec!["id".to_string()],
                            rows: vec![vec![Some("41".to_string())]],
                        })
                    });
                while let Some(event) = session.next_event().expect("event") {
                    events.push(event);
                }
            }
            events
        });
        (address, handle)
    }

    #[test]
    fn an_archived_item_is_one_insert_and_its_receipt_names_the_row() {
        let original = item("json#1");
        let (address, far_end) = far_end(Some("secret"), &original, 1);
        let store = PostgresqlArchive::new(address.clone(), "orders", "xmip")
            .with_password("secret")
            .with_table("audit.archive")
            .timing_out_after(secs(2));
        let receipt = store.archive(original.clone()).expect("archive");
        assert_eq!(
            receipt.location,
            format!("postgresql://{address}/orders/audit.archive?id=41")
        );
        assert_eq!(receipt.checksum, None);
        let events = far_end.join().expect("thread");
        assert_eq!(events.len(), 1, "one statement, then the client closed");
        let Event::Executed(sql) = &events[0] else {
            panic!("an INSERT is answered by the closure: {:?}", events[0]);
        };
        assert!(sql.starts_with("INSERT INTO \"audit\".\"archive\" (data_type, identifier, "));
        assert!(sql.contains("VALUES ('json', 'json#1', '\\x7b226b657074223a747275657d', "));
        assert!(sql.contains("'source\u{1f}playground', '20"));
        assert!(sql.ends_with("Z') RETURNING id"), "{sql}");
    }

    #[test]
    fn the_row_restores_the_item_over_a_second_connection() {
        let original = item("json#2");
        let (address, far_end) = far_end(None, &original, 2);
        let store = PostgresqlArchive::new(address, "orders", "xmip").timing_out_after(secs(2));
        let receipt = store.archive(original.clone()).expect("archive");
        let restored = store.restore(&receipt).expect("restore");
        assert_eq!(restored, original, "the row read back is the item");
        let events = far_end.join().expect("thread");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1],
            Event::Selected(
                "SELECT data_type, identifier, bytes, metadata FROM \"archive\" WHERE id = 41"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_wrong_password_and_a_wrong_receipt_are_refused() {
        let (address, far_end) = far_end(Some("secret"), &item("json#3"), 1);
        let store = PostgresqlArchive::new(address, "orders", "xmip")
            .with_password("wrong")
            .timing_out_after(secs(2));
        let refused = store.archive(item("json#3")).expect_err("wrong password");
        assert!(refused.message.contains("28P01"), "{refused}");
        far_end.join().expect("thread");
        for location in [
            "s3://bucket/key",
            "postgresql://host/orders/archive",
            "postgresql://host/orders/archive?id=x",
            "postgresql://host/orders?id=1",
        ] {
            let receipt = ArchiveReceipt {
                location: location.to_string(),
                checksum: None,
            };
            let failure = store.restore(&receipt).expect_err(location);
            assert!(
                failure.message.contains("is not postgresql://"),
                "{failure}"
            );
        }
    }
}
