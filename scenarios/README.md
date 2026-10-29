# Scenarios

Each `.toml` file here is one schema change that `kilit-lab` measures. The
file name, without `.toml`, is the scenario's id, so ids are unique by
construction. Use lowercase letters, digits and single hyphens.

```toml
title = "Add a foreign key with foreign_key_checks off"

# Optional: statements run in the same session right before the ALTER.
session = ["SET foreign_key_checks = 0"]

# Everything that follows `ALTER TABLE orders`.
operation = "ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id)"
```

Every run starts from fresh `users` and `orders` tables (see
`crates/kilit-lab/src/fixture.rs`):

| Table    | Rows | What scenarios can rely on                                              |
| -------- | ---- | ----------------------------------------------------------------------- |
| `users`  | 100  | `id` 1–100                                                              |
| `orders` | 1000 | `user_id` repeats (1–100); `email` is filled in every row and unique; `note` is NULL in every 10th row |

Adding a scenario needs no Rust change: add a file, then run
`cargo test` (it checks that every file here loads) and the lab.
