//! Versioned schema migrations, tracked in `PRAGMA user_version`.
//!
//! Rules: never edit or reorder a migration that has shipped; append a new
//! one instead. Each migration runs in a transaction.

use rusqlite_migration::{Migrations, M};

pub fn migrations() -> Migrations<'static> {
    Migrations::new(all())
}

/// The schema version of a fully migrated database.
pub fn latest_version() -> u32 {
    all().len() as u32
}

fn all() -> Vec<M<'static>> {
    vec![
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
        // v4 — requirement profiles: what "good" means for a project, which
        // networks it applies to, per-floor overrides, and what bands the
        // measuring card could receive (NULL on older points = not recorded).
        M::up(
            "CREATE TABLE requirement_profiles (
                id                  INTEGER PRIMARY KEY,
                project_id          INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                name                TEXT    NOT NULL CHECK (length(trim(name)) > 0),
                preset              TEXT    NOT NULL
                    CHECK (preset IN ('office_data', 'voice_video', 'warehouse_basic', 'custom')),
                primary_min_dbm     INTEGER NOT NULL CHECK (primary_min_dbm BETWEEN -100 AND -20),
                secondary_min_dbm   INTEGER CHECK (secondary_min_dbm BETWEEN -100 AND -20),
                cochannel_max       INTEGER CHECK (cochannel_max BETWEEN 0 AND 50),
                cochannel_level_dbm INTEGER CHECK (cochannel_level_dbm BETWEEN -100 AND -20),
                required_bands      TEXT    NOT NULL DEFAULT '',  -- comma-separated: '2.4ghz,5ghz,6ghz'
                min_snr_db          INTEGER CHECK (min_snr_db BETWEEN 0 AND 80),
                max_util_pct        INTEGER CHECK (max_util_pct BETWEEN 0 AND 100),
                is_default          INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
                created_at          TEXT    NOT NULL,
                updated_at          TEXT    NOT NULL,
                CHECK ((cochannel_max IS NULL) = (cochannel_level_dbm IS NULL)),
                UNIQUE (project_id, name)
            ) STRICT;
            CREATE UNIQUE INDEX idx_requirement_profiles_default
                ON requirement_profiles(project_id) WHERE is_default = 1;

            -- The networks a profile applies to: an SSID (raw bytes) or a
            -- placed AP (all its BSSIDs, so hidden SSIDs work). Exactly one.
            CREATE TABLE requirement_profile_targets (
                id            INTEGER PRIMARY KEY,
                profile_id    INTEGER NOT NULL REFERENCES requirement_profiles(id) ON DELETE CASCADE,
                ssid_raw      BLOB    CHECK (ssid_raw IS NULL OR length(ssid_raw) BETWEEN 1 AND 32),
                placed_ap_id  INTEGER REFERENCES placed_aps(id) ON DELETE CASCADE,
                CHECK ((ssid_raw IS NULL) <> (placed_ap_id IS NULL))
            ) STRICT;
            CREATE INDEX idx_requirement_targets_profile ON requirement_profile_targets(profile_id);
            CREATE INDEX idx_requirement_targets_ap ON requirement_profile_targets(placed_ap_id);
            CREATE UNIQUE INDEX idx_requirement_targets_ssid
                ON requirement_profile_targets(profile_id, ssid_raw) WHERE ssid_raw IS NOT NULL;
            CREATE UNIQUE INDEX idx_requirement_targets_unique_ap
                ON requirement_profile_targets(profile_id, placed_ap_id) WHERE placed_ap_id IS NOT NULL;

            -- A whole profile per floor (NULL = the project's default).
            ALTER TABLE floors ADD COLUMN requirement_profile_id INTEGER
                REFERENCES requirement_profiles(id) ON DELETE SET NULL;
            CREATE INDEX idx_floors_requirement_profile ON floors(requirement_profile_id);

            -- JSON {band2ghz, band5ghz, band6ghz}: 'supported' | 'unsupported' | 'unknown'.
            ALTER TABLE survey_points ADD COLUMN adapter_bands TEXT
                CHECK (adapter_bands IS NULL OR json_valid(adapter_bands));",
        ),
        // v5 — notes, note pins, photos. Free-text notes on floors and points
        // (placed APs have had `notes` since v3); pins with a note anywhere on
        // a plan; photos attached to a floor, or to one point, AP or pin on
        // it. Lengths are capped in Rust. Photo files live in the app's
        // photos/ directory; the row names all three (original, report copy,
        // thumbnail).
        M::up(
            "ALTER TABLE floors ADD COLUMN notes TEXT;
            ALTER TABLE survey_points ADD COLUMN notes TEXT;

            CREATE TABLE note_pins (
                id          INTEGER PRIMARY KEY,
                floor_id    INTEGER NOT NULL REFERENCES floors(id) ON DELETE CASCADE,
                x           REAL    NOT NULL,   -- plan pixels, like survey points
                y           REAL    NOT NULL,
                text        TEXT    NOT NULL CHECK (length(trim(text)) > 0),
                category    TEXT,               -- e.g. 'obstruction', 'interference'
                created_at  TEXT    NOT NULL,
                updated_at  TEXT    NOT NULL
            ) STRICT;
            CREATE INDEX idx_note_pins_floor ON note_pins(floor_id);

            CREATE TABLE photos (
                id           INTEGER PRIMARY KEY,
                floor_id     INTEGER NOT NULL REFERENCES floors(id) ON DELETE CASCADE,
                point_id     INTEGER REFERENCES survey_points(id) ON DELETE CASCADE,
                ap_id        INTEGER REFERENCES placed_aps(id) ON DELETE CASCADE,
                pin_id       INTEGER REFERENCES note_pins(id) ON DELETE CASCADE,
                file         TEXT    NOT NULL,  -- original as imported (keeps its metadata)
                report_file  TEXT    NOT NULL,  -- downscaled JPEG without metadata
                thumb_file   TEXT    NOT NULL,
                width        INTEGER NOT NULL,  -- original, upright
                height       INTEGER NOT NULL,
                taken_at     TEXT,              -- EXIF capture time; offset only if recorded
                had_gps      INTEGER NOT NULL,  -- the original's EXIF has a position
                caption      TEXT,
                in_report    INTEGER NOT NULL DEFAULT 1 CHECK (in_report IN (0, 1)),
                created_at   TEXT    NOT NULL,
                CHECK ((point_id IS NOT NULL) + (ap_id IS NOT NULL) + (pin_id IS NOT NULL) <= 1)
            ) STRICT;
            CREATE INDEX idx_photos_floor ON photos(floor_id);
            CREATE INDEX idx_photos_point ON photos(point_id);
            CREATE INDEX idx_photos_ap ON photos(ap_id);
            CREATE INDEX idx_photos_pin ON photos(pin_id);",
        ),
        // v6 — security details, BSSID marks (rogue / evil-twin detection).
        // New sample columns are NULL on rows measured before this version
        // ("not recorded"). From v6 on `hidden` is always set, so it tells
        // the two apart; a NULL pmf / group_mgmt_cipher on a newer row means
        // the source didn't say. akms, pairwise and group_cipher are JSON
        // arrays and group_mgmt_cipher one JSON value, as serde writes the
        // Akm / Cipher enums (e.g. ["psk","sae"]; unknown suites as numbers).
        M::up(
            "ALTER TABLE survey_samples ADD COLUMN akms TEXT;
            ALTER TABLE survey_samples ADD COLUMN pairwise TEXT;
            ALTER TABLE survey_samples ADD COLUMN group_cipher TEXT;
            ALTER TABLE survey_samples ADD COLUMN group_mgmt_cipher TEXT;
            ALTER TABLE survey_samples ADD COLUMN pmf TEXT
                CHECK (pmf IN ('disabled', 'capable', 'required'));
            ALTER TABLE survey_samples ADD COLUMN hidden INTEGER;
            ALTER TABLE survey_samples ADD COLUMN mld_addr TEXT;   -- uppercase, colon-separated

            -- Readings Measure Here set aside that are evidence in themselves:
            -- one BSSID listed on another frequency in the same scan (the kept
            -- reading is the sample).
            CREATE TABLE point_anomalies (
                point_id        INTEGER NOT NULL REFERENCES survey_points(id) ON DELETE CASCADE,
                bssid           TEXT    NOT NULL,
                kind            TEXT    NOT NULL CHECK (kind IN ('multi_frequency')),
                frequency_mhz   INTEGER NOT NULL,
                channel         INTEGER,
                band            TEXT    NOT NULL,
                ssid_raw        BLOB    NOT NULL,
                signal_dbm      REAL,
                signal_percent  INTEGER,
                PRIMARY KEY (point_id, bssid, kind, frequency_mhz)
            ) STRICT, WITHOUT ROWID;

            -- The user's classification of a BSSID, once per project.
            CREATE TABLE bssid_marks (
                project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                bssid       TEXT    NOT NULL,   -- uppercase, colon-separated
                status      TEXT    NOT NULL
                    CHECK (status IN ('ours_unplaced', 'neighbour', 'ignored')),
                note        TEXT,
                updated_at  TEXT    NOT NULL,
                PRIMARY KEY (project_id, bssid)
            ) STRICT, WITHOUT ROWID;",
        ),
        // v7 — active tests per point (ping, iperf3), apart from the RF
        // samples: a failed target never touches a measurement. The link
        // at the start is in columns (report tables, filters); the link at
        // the end and the results are versioned JSON. roamed NULL: unknown.
        M::up(
            "CREATE TABLE point_tests (
                id          INTEGER PRIMARY KEY,
                point_id    INTEGER NOT NULL REFERENCES survey_points(id) ON DELETE CASCADE,
                kind        TEXT    NOT NULL CHECK (kind IN ('ping', 'iperf3')),
                target      TEXT    NOT NULL,
                role        TEXT    NOT NULL
                            CHECK (role IN ('gateway', 'extra_host', 'iperf3_upload', 'iperf3_download')),
                method      TEXT    NOT NULL CHECK (method IN ('icmp', 'tcp_connect', 'iperf3_tcp')),
                status      TEXT    NOT NULL CHECK (status IN ('ok', 'failed', 'cancelled')),
                started_at  TEXT    NOT NULL,
                duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
                adapter_id  TEXT    NOT NULL,
                connected   INTEGER NOT NULL CHECK (connected IN (0, 1)),
                iface       TEXT,
                bssid       TEXT,
                freq        INTEGER,
                signal_dbm  REAL,
                tx_kbps     INTEGER,
                rx_kbps     INTEGER,
                phy         TEXT,
                mcs         INTEGER,
                nss         INTEGER,
                width_mhz   INTEGER,
                link_after_json TEXT CHECK (link_after_json IS NULL OR json_valid(link_after_json)),
                roamed      INTEGER CHECK (roamed IN (0, 1)),
                results_json TEXT CHECK (results_json IS NULL OR json_valid(results_json)),
                error       TEXT,
                error_hint  TEXT
            ) STRICT;
            CREATE INDEX idx_point_tests_point ON point_tests(point_id, started_at);",
        ),
        // v8 — Tools page runs (ping, traceroute, DNS, port check, iperf3).
        // Not tied to a project; a run can be attached to a survey point
        // afterwards (deleting the point detaches it). Results are
        // versioned JSON; params are what the UI sent, for "run again".
        M::up(
            "CREATE TABLE tool_runs (
                id          INTEGER PRIMARY KEY,
                kind        TEXT    NOT NULL
                            CHECK (kind IN ('ping', 'traceroute', 'dns', 'port_check', 'iperf3')),
                target      TEXT    NOT NULL,
                resolved_ip TEXT,
                params_json TEXT    NOT NULL CHECK (json_valid(params_json)),
                status      TEXT    NOT NULL CHECK (status IN ('ok', 'failed', 'stopped')),
                started_at  TEXT    NOT NULL,
                duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
                route_iface TEXT,
                adapter_id  TEXT,
                link_json   TEXT CHECK (link_json IS NULL OR json_valid(link_json)),
                link_after_json TEXT CHECK (link_after_json IS NULL OR json_valid(link_after_json)),
                roamed      INTEGER CHECK (roamed IN (0, 1)),
                summary     TEXT,
                results_json TEXT CHECK (results_json IS NULL OR json_valid(results_json)),
                error       TEXT,
                error_hint  TEXT,
                point_id    INTEGER REFERENCES survey_points(id) ON DELETE SET NULL
            ) STRICT;
            CREATE INDEX idx_tool_runs_kind ON tool_runs(kind, started_at);
            CREATE INDEX idx_tool_runs_point ON tool_runs(point_id) WHERE point_id IS NOT NULL;",
        ),
        // v9 — speed targets in requirement profiles, judged on the points'
        // active tests. NULL: the rule is off (every existing profile).
        M::up(
            "ALTER TABLE requirement_profiles ADD COLUMN min_download_mbps INTEGER
                CHECK (min_download_mbps IS NULL OR min_download_mbps BETWEEN 1 AND 100000);
            ALTER TABLE requirement_profiles ADD COLUMN min_upload_mbps INTEGER
                CHECK (min_upload_mbps IS NULL OR min_upload_mbps BETWEEN 1 AND 100000);
            ALTER TABLE requirement_profiles ADD COLUMN max_latency_ms INTEGER
                CHECK (max_latency_ms IS NULL OR max_latency_ms BETWEEN 1 AND 10000);
            ALTER TABLE requirement_profiles ADD COLUMN max_loss_pct INTEGER
                CHECK (max_loss_pct IS NULL OR max_loss_pct BETWEEN 0 AND 100);",
        ),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrations_are_valid() {
        assert!(super::migrations().validate().is_ok());
    }
}
