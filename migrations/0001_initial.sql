CREATE TABLE IF NOT EXISTS assets (
    id           TEXT    PRIMARY KEY,
    name         TEXT    NOT NULL,
    type         TEXT    NOT NULL CHECK(type IN ('ONVIF','HL7MONITOR','VENTILATOR')),
    description  TEXT    NOT NULL DEFAULT '',
    ip_address   TEXT    NOT NULL,
    port         INTEGER NOT NULL DEFAULT 80,
    username     TEXT,
    password_enc BLOB,
    access_key   TEXT,
    deleted      INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS assets_ip ON assets(ip_address);
CREATE INDEX IF NOT EXISTS assets_type ON assets(type, deleted);

CREATE TABLE IF NOT EXISTS daily_rounds (
    id               TEXT    PRIMARY KEY,
    asset_id         TEXT    NOT NULL REFERENCES assets(id),
    asset_external_id TEXT   NOT NULL,
    status           TEXT    NOT NULL,
    data             TEXT    NOT NULL,
    response         TEXT    NOT NULL DEFAULT '',
    time             TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS daily_rounds_asset ON daily_rounds(asset_external_id);
