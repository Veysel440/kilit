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
```

Every run starts from fresh `users` and `orders` tables (see
`crates/kilit-lab/src/fixture.rs`):

| Table    | Rows | What scenarios can rely on                                              |
| -------- | ---- | ----------------------------------------------------------------------- |
| `users`  | 100  | `id` 1–100                                                              |
| `orders` | 1000 | `user_id` repeats (1–100); `email` is filled in every row and unique; `note` is NULL in every 10th row |

## Adding a scenario

1. Add a file here and run `cargo test`. It checks that the file loads and
   that kilit-core can read the statement the lab will send.
2. Run the lab. It reports how the results compare with the saved ones.
3. Run `cargo test` again. Every measured scenario must be used by a case
   in `crates/kilit-core/src/check.rs`, as one of the case's scenarios or
   as an exception; the test says which one is missing.
