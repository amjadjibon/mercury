CREATE TABLE IF NOT EXISTS latency_snapshots (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    p50_ns    INTEGER NOT NULL,
    p99_ns    INTEGER NOT NULL,
    p999_ns   INTEGER NOT NULL,
    timestamp INTEGER NOT NULL
);

CREATE INDEX idx_latency_snapshots_timestamp ON latency_snapshots (timestamp);
