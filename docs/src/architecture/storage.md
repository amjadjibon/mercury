# Storage

`StorageManager` persists `Fill` events to a SQLite database using sqlx.

## Schema

Migrations live in `crates/storage/migrations/`. The current schema has one table:

```sql
CREATE TABLE trades (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id    TEXT    NOT NULL,
    symbol      TEXT    NOT NULL,
    exchange    TEXT    NOT NULL,
    side        TEXT    NOT NULL,  -- 'buy' | 'sell'
    price       TEXT    NOT NULL,  -- stored as decimal string
    quantity    TEXT    NOT NULL,
    fee         TEXT    NOT NULL,
    timestamp   INTEGER NOT NULL   -- Unix millis
);
```

Prices and quantities are stored as decimal strings to preserve exact precision.

## Configuration

Default database path: `mercury.db` in the working directory.

Use `:memory:` for tests:

```rust
StorageManager::new(":memory:").await?;
```

## Adding new tables

1. Create a new migration file in `crates/storage/migrations/` following the `NNNN_description.sql` naming convention.
2. sqlx applies migrations automatically on `StorageManager` startup.
