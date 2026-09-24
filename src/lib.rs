#![forbid(unsafe_code)]

//! `PostgreSQL` archive: the [`Dialect`] the archive capability's
//! `SqlArchive` keeps each retained item in an archive table with, as
//! `SqlArchive::<PostgreSql>`.
//!
//! The store, the row, the SELECT and the receipt are the capability's
//! (`archive::sql`, ADR-0044); only the dialect is this crate's —
//! double-quoted identifiers, ISO string literals, the bytes in the bytea
//! hex form the transport speaks, which a `bytea` column reads as the
//! bytes and a `text` column keeps verbatim (either comes back as the
//! bytes), and the new row's id asked for with `RETURNING`. The
//! connection is the `PostgreSQL` transport technology's — the simple
//! query flow, trust or a cleartext password. The table is the operator's
//! to create; this is the shape it is written for:
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
//! The receipt is `postgresql://<server>/<database>/<table>?id=<n>`.

use archive::ArchiveError;
use archive::sql::{Dialect, Row, Server};
use postgresql::{Client, bytea};

/// What `PostgreSQL` does its own way.
pub struct PostgreSql;

impl Dialect for PostgreSql {
    const SCHEME: &'static str = "postgresql";
    const ID_AFTER_VALUES: &'static str = " RETURNING id";
    type Connection = Client;

    fn quote_identifier(name: &str) -> String {
        postgresql::quote_identifier(name)
    }

    fn quote_literal(text: &str) -> String {
        postgresql::quote_literal(text)
    }

    fn bytes_literal(bytes: &[u8]) -> String {
        postgresql::quote_literal(&bytea::hex_literal(bytes))
    }

    fn column_bytes(text: String) -> Vec<u8> {
        bytea::column_bytes(text)
    }

    fn connect(server: &Server) -> Result<Client, ArchiveError> {
        Client::connect(
            &server.address,
            &server.user,
            &server.database,
            server.password.as_deref(),
            server.timeout,
        )
        .map_err(ArchiveError::caused_by)
    }

    fn select(client: &mut Client, sql: &str) -> Result<Vec<Row>, ArchiveError> {
        Ok(client.query(sql).map_err(ArchiveError::caused_by)?.rows)
    }

    fn close(client: Client) -> Result<(), ArchiveError> {
        client.close().map_err(ArchiveError::caused_by)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archive::fixture::{item, secs};
    use archive::sql::{SqlArchive, insert_sql, item_from_row, select_sql};
    use archive::{ArchiveItem, ArchiveReceipt, ArchiveStore, metadata};
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
            Some(metadata::encode(&held.metadata)),
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

    fn quoted() -> ArchiveItem {
        ArchiveItem {
            data_type: "json".to_string(),
            identifier: "it's #1".to_string(),
            bytes: vec![0x7b, 0xff],
            metadata: vec![("source".to_string(), "playground".to_string())],
        }
    }

    #[test]
    fn the_insert_names_the_five_columns_and_asks_for_the_id() {
        let sql = insert_sql::<PostgreSql>("audit.archive", &quoted(), "2026-09-09T12:00:00Z");
        assert!(sql.starts_with(
            "INSERT INTO \"audit\".\"archive\" \
             (data_type, identifier, bytes, metadata, archived_at) VALUES ('json', 'it''s #1', \
             '\\x7bff', "
        ));
        assert!(sql.ends_with("'2026-09-09T12:00:00Z') RETURNING id"));
        assert_eq!(
            select_sql::<PostgreSql>("Archive", 41),
            "SELECT data_type, identifier, bytes, metadata FROM \"Archive\" WHERE id = 41"
        );
    }

    #[test]
    fn a_row_in_either_bytes_form_is_the_item_again() {
        let original = quoted();
        let hex = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("\\x7bff".to_string()),
            Some(metadata::encode(&original.metadata)),
        ];
        let restored = item_from_row::<PostgreSql>(&hex, "here").expect("row");
        assert_eq!(restored, original);
        let text = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("plain".to_string()),
            Some(String::new()),
        ];
        let restored = item_from_row::<PostgreSql>(&text, "here").expect("row");
        assert_eq!(restored.bytes, b"plain");
        assert!(restored.metadata.is_empty());
    }

    #[test]
    fn an_archived_item_is_one_insert_and_its_receipt_names_the_row() {
        let original = item("json#1");
        let (address, far_end) = far_end(Some("secret"), &original, 1);
        let store = SqlArchive::<PostgreSql>::new(address.clone(), "orders", "xmip")
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
        let store =
            SqlArchive::<PostgreSql>::new(address, "orders", "xmip").timing_out_after(secs(2));
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
        let store = SqlArchive::<PostgreSql>::new(address, "orders", "xmip")
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
