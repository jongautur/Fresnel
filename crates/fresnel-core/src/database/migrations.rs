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
        // v2 — survey structure: buildings, floors (plan + scale), points, samples.
        // Coordinates are in the floor plan's pixel space (the image's natural
        // size as rendered); metres come from the floor's scale line.
        M::up(
            "CREATE TABLE buildings (
                id          INTEGER PRIMARY KEY,
                project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                name        TEXT    NOT NULL CHECK (length(trim(name)) > 0),
                created_at  TEXT    NOT NULL,
                updated_at  TEXT    NOT NULL
            ) STRICT;
            CREATE INDEX idx_buildings_project ON buildings(project_id);

            CREATE TABLE floors (
                id              INTEGER PRIMARY KEY,
                building_id     INTEGER NOT NULL REFERENCES buildings(id) ON DELETE CASCADE,
                name            TEXT    NOT NULL CHECK (length(trim(name)) > 0),
                level           INTEGER NOT NULL DEFAULT 0,  -- sort order: -1 basement, 0 ground, 1 ...
                plan_file       TEXT,     -- file name inside the app's floorplans/ directory
                plan_mime       TEXT,
                plan_width      REAL,
                plan_height     REAL,
                scale_x1        REAL,     -- reference line drawn on the plan ...
                scale_y1        REAL,
                scale_x2        REAL,
                scale_y2        REAL,
                scale_length_m  REAL CHECK (scale_length_m IS NULL OR scale_length_m > 0),  -- ... and its real length
                created_at      TEXT    NOT NULL,
                updated_at      TEXT    NOT NULL,
                CHECK ((plan_file IS NULL) = (plan_width IS NULL)),
                CHECK ((scale_length_m IS NULL) OR (plan_file IS NOT NULL))
            ) STRICT;
            CREATE INDEX idx_floors_building ON floors(building_id);

            -- One 'Measure Here': where, when, and which radio measured.
            CREATE TABLE survey_points (
                id                INTEGER PRIMARY KEY,
                floor_id          INTEGER NOT NULL REFERENCES floors(id) ON DELETE CASCADE,
                x                 REAL    NOT NULL,
                y                 REAL    NOT NULL,
                measured_at       TEXT    NOT NULL,
                scan_duration_ms  INTEGER NOT NULL,
                provider          TEXT    NOT NULL,
                adapter_id        TEXT    NOT NULL,
                adapter_model     TEXT,   -- e.g. 'Wireless-AC 9560'; per-model calibration keys on this
                adapter_driver    TEXT,
                adapter_hw_id     TEXT    -- e.g. 'pci:8086:a370'
            ) STRICT;
            CREATE INDEX idx_points_floor ON survey_points(floor_id);

            -- Raw readings of every BSSID heard during that point's scan.
            CREATE TABLE survey_samples (
                point_id                INTEGER NOT NULL REFERENCES survey_points(id) ON DELETE CASCADE,
                bssid                   TEXT    NOT NULL,
                ssid                    TEXT,
                ssid_raw                BLOB    NOT NULL,
                frequency_mhz           INTEGER NOT NULL,
                channel                 INTEGER,
                band                    TEXT    NOT NULL,
                channel_width_mhz       INTEGER,
                channel_center_mhz      INTEGER,
                signal_dbm              REAL,
                signal_percent          INTEGER,
                security                TEXT    NOT NULL,
                phy_type                TEXT,
                wifi_generation         INTEGER,
                noise_dbm               REAL,
                snr_db                  REAL,
                channel_utilization_pct REAL,
                station_count           INTEGER,
                last_seen_age_ms        INTEGER,
                is_connected            INTEGER NOT NULL,
                PRIMARY KEY (point_id, bssid)
            ) STRICT, WITHOUT ROWID;
            CREATE INDEX idx_samples_bssid ON survey_samples(bssid);",
        ),
        // v3 — access points placed on floor plans, and the BSSIDs each one
        // broadcasts (one physical AP usually has several: bands, SSIDs, MLO).
        M::up(
            "CREATE TABLE placed_aps (
                id          INTEGER PRIMARY KEY,
                floor_id    INTEGER NOT NULL REFERENCES floors(id) ON DELETE CASCADE,
                name        TEXT    NOT NULL CHECK (length(trim(name)) > 0),
                x           REAL    NOT NULL,   -- plan pixels, like survey points
                y           REAL    NOT NULL,
                model       TEXT,
                notes       TEXT,
                created_at  TEXT    NOT NULL,
                updated_at  TEXT    NOT NULL
            ) STRICT;
            CREATE INDEX idx_placed_aps_floor ON placed_aps(floor_id);

            CREATE TABLE placed_ap_bssids (
                ap_id   INTEGER NOT NULL REFERENCES placed_aps(id) ON DELETE CASCADE,
                bssid   TEXT    NOT NULL,   -- uppercase, colon-separated
                PRIMARY KEY (ap_id, bssid)
            ) STRICT, WITHOUT ROWID;
            CREATE INDEX idx_placed_ap_bssids_bssid ON placed_ap_bssids(bssid);",
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
