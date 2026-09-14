//! One item as one row, the `PostgreSQL` part: the INSERT that stores it
//! and asks for the id with `RETURNING`, and the dialect the shared row
//! code is given — double-quoted identifiers, and the bytes in the bytea
//! hex form the transport speaks, which a `bytea` column reads as the
//! bytes and a `text` column keeps verbatim; either comes back as the
//! bytes. The columns, the SELECT and the row read back are the
//! capability's `archive::row` (ADR-0044).

use archive::row::Dialect;
use archive::{ArchiveItem, metadata};
use postgresql::{bytea, quote_identifier, quote_literal};

/// What `PostgreSQL` does its own way, handed to the shared row code.
pub const DIALECT: Dialect = Dialect {
    quote_identifier,
    bytes_expression: "bytes",
    column_bytes: bytea::column_bytes,
};

/// The statement that stores `item` in `table` at `archived_at`, answering
/// with the new row's `id`.
#[must_use]
pub fn insert_sql(table: &str, item: &ArchiveItem, archived_at: &str) -> String {
    format!(
        "INSERT INTO {} (data_type, identifier, bytes, metadata, archived_at) \
         VALUES ({}, {}, {}, {}, {}) RETURNING id",
        DIALECT.table_name(table),
        quote_literal(&item.data_type),
        quote_literal(&item.identifier),
        quote_literal(&bytea::hex_literal(&item.bytes)),
        quote_literal(&metadata::encode(&item.metadata)),
        quote_literal(archived_at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> ArchiveItem {
        ArchiveItem {
            data_type: "json".to_string(),
            identifier: "it's #1".to_string(),
            bytes: vec![0x7b, 0xff],
            metadata: vec![("source".to_string(), "playground".to_string())],
        }
    }

    #[test]
    fn the_insert_names_the_five_columns_and_asks_for_the_id() {
        let sql = insert_sql("audit.archive", &item(), "2026-09-09T12:00:00Z");
        assert!(sql.starts_with(
            "INSERT INTO \"audit\".\"archive\" \
             (data_type, identifier, bytes, metadata, archived_at) VALUES ('json', 'it''s #1', \
             '\\x7bff', "
        ));
        assert!(sql.ends_with("'2026-09-09T12:00:00Z') RETURNING id"));
        assert_eq!(
            DIALECT.select_sql("Archive", 41),
            "SELECT data_type, identifier, bytes, metadata FROM \"Archive\" WHERE id = 41"
        );
    }

    #[test]
    fn a_row_in_either_bytes_form_is_the_item_again() {
        let original = item();
        let hex = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("\\x7bff".to_string()),
            Some(metadata::encode(&original.metadata)),
        ];
        assert_eq!(DIALECT.item_from_row(&hex, "here").expect("row"), original);
        let text = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("plain".to_string()),
            Some(String::new()),
        ];
        let restored = DIALECT.item_from_row(&text, "here").expect("row");
        assert_eq!(restored.bytes, b"plain");
        assert!(restored.metadata.is_empty());
    }
}
