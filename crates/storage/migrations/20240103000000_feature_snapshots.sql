CREATE TABLE IF NOT EXISTS feature_snapshots (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   INTEGER NOT NULL,
    symbol      TEXT NOT NULL,
    f0          REAL,
    f1          REAL,
    f2          REAL,
    f3          REAL,
    f4          REAL,
    f5          REAL,
    f6          REAL,
    f7          REAL,
    f8          REAL,
    f9          REAL,
    sell_prob   REAL NOT NULL,
    buy_prob    REAL NOT NULL,
    decision    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_feature_snapshots_symbol_ts ON feature_snapshots (symbol, timestamp);
