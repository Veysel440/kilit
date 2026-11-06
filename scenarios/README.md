# Scenarios

Each `.toml` file here is one schema change that `kilit-lab` measures. The
file name, without `.toml`, is the scenario's id, so ids are unique by
construction. Use lowercase letters, digits and single hyphens.

```toml
title = "Add a foreign key when a row points to a missing parent"

# Optional: statements that prepare the tables before anything is
# measured, e.g. the index a scenario then drops, or data it needs.
setup = ["INSERT INTO orders (user_id, amount) VALUES (9999, 1)"]

# Optional: statements run in the same session right before the ALTER.
session = ["SET foreign_key_checks = 1"]

# Everything that follows `ALTER TABLE orders`.
operation = "ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id)"

# Optional: "MyISAM" creates `orders` with ENGINE=MyISAM instead of
# ENGINE=InnoDB. Nothing else about the tables changes.
engine = "MyISAM"
```

Every run starts from fresh `users` and `orders` tables, defined in
[`tables.sql`](tables.sql) and filled by `crates/kilit-lab/src/fixture.rs`.
kilit-core's tests read `tables.sql` and each scenario's `setup` too, so
they judge every scenario on the tables it was measured on.

| Table    | Rows | What scenarios can rely on                                              |
| -------- | ---- | ----------------------------------------------------------------------- |
| `users`  | 100  | `id` 1–100                                                              |
| `orders` | 1000 | `user_id` repeats (1–100); `amount` is 1–1000; `email` is filled in every row and unique; `note` looks like `note 123` and is NULL in every 10th row |

Both tables are `utf8mb4`, so a character can take up to 4 bytes. Both
are InnoDB, unless a scenario makes `orders` MyISAM with `engine`. MySQL
gives a MyISAM table no table id, so the lab tells a copy of it from the
server's count of its `copy to tmp table` stage, and checks that against
the `Records: N` it reports.

## Adding a scenario

1. Add a file here and run `cargo test`. It checks that the file loads and
   that kilit-core can read the statement the lab will send.
2. Run the lab. It reports how the results compare with the saved ones.
3. Run `cargo test` again. Every measured scenario must be used by a case
   in `crates/kilit-core/src/check.rs`, as one of the case's scenarios or
   as an exception; the test says which one is missing.
