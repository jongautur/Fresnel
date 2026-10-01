//! Versioned schema migrations, tracked in `PRAGMA user_version`.
//!
//! Rules: never edit or reorder a migration that has shipped; append a new
//! one instead. Each migration runs in a transaction.

use rusqlite_migration::{Migrations, M};

pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        // v1 — projects (survey projects; buildings/floors/samples come later)
        M::up(
            "CREATE TABLE projects (
                id            INTEGER PRIMARY KEY,
                name          TEXT    NOT NULL CHECK (length(trim(name)) > 0),
                customer_name TEXT,
                created_at    TEXT    NOT NULL,  -- RFC 3339 UTC
                updated_at    TEXT    NOT NULL
            ) STRICT;
            CREATE INDEX idx_projects_updated_at ON projects(updated_at);",
        ),
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrations_are_valid() {
        assert!(super::migrations().validate().is_ok());
    }
}
