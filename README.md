# kilit

kilit will tell you, before you deploy, what MySQL is going to do with a schema
migration: change it instantly, rebuild the table, or copy every row while
writes wait. Its answers will come from measurements on real servers, not from
guesses.

> **Status:** early development. Only the measurement lab exists so far.

## The lab

`kilit-lab` runs each scenario against real MySQL servers in several ways
(server default, `ALGORITHM=INSTANT|INPLACE|COPY`, `LOCK=NONE`) and records
what the server did:

- whether it accepted the statement, and the error code if it did not
- how many rows it copied (`Records: N` in the server's reply)
- whether InnoDB rebuilt the table (the table's internal id changed)
- InnoDB's counter of instant column changes (`TOTAL_ROW_VERSIONS`)

### Run it

You need Rust 1.88 or newer and Docker with Compose.

```sh
docker compose up -d --wait   # MySQL 8.0.46 on port 3380, 8.4.13 on port 3384
cargo run -p kilit-lab
docker compose down           # remove the servers when you are done
```

The lab drops and recreates a database named `kilit_lab`. Only point it at
throwaway servers.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
