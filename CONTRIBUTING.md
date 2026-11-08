# Contributing

The most valuable contribution is a new measurement. This page covers how to
add one, how to run the lab, what to run before a pull request, and the rules
the code follows.

- [Adding a measurement](#adding-a-measurement)
- [Running the lab](#running-the-lab)
- [Before a pull request](#before-a-pull-request)
- [Rules of the code](#rules-of-the-code)
- [Project layout](#project-layout)
- [License of contributions](#license-of-contributions)

## Adding a measurement

A scenario is a small TOML file in [`scenarios/`](scenarios/):

```toml
title = "Add a FULLTEXT index"
operation = "ADD FULLTEXT INDEX ft_orders_note (note)"
```

[`scenarios/README.md`](scenarios/README.md) explains the format. From a new
file to an answer kilit gives:

1. Add `scenarios/<id>.toml` and run `cargo test`. It checks that the file
   loads and that kilit-core can read the statement the lab will send.
2. Run the lab (below). Each server should report the new scenario as new and
   every earlier one as unchanged, and say that kilit-core's facts hold. The
   files in `matrix/` change by additions only.
3. Bind the scenario to a case or an exception in
   `crates/kilit-core/src/check.rs`, then run `cargo test` again. Every
   measured scenario must be used by a case, as one of its scenarios or as an
   exception; the test `every_scenario_is_used` says which one is missing.

If the result surprises you, or the variants or the servers disagree in a way
you did not expect, do not add the scenario: open an issue with the result
instead.

## Running the lab

The lab needs Docker with Compose.

```sh
docker compose up -d --wait          # MySQL 8.0.46 on port 3380, 8.4.13 on port 3384
cargo run -p kilit-lab               # a summary per server; results go to matrix/
cargo run -p kilit-lab -- --details  # also every variant of every scenario
docker compose down                  # remove the servers
```

> [!WARNING]
> The lab drops and recreates a database named `kilit_lab`. Only point it at
> throwaway servers.

## Before a pull request

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo +1.88 test --workspace   # the oldest Rust kilit supports
cargo tree -i cc               # must print nothing
```

Tests are named as sentences that state the behavior, such as
`dropping_a_foreign_key_column_fails`, and their inputs are written by hand.
The documentation is tested too: the examples in `README.md` and
`docs/json-output.md` must be the real output of the command shown, and every
relative link must point at a file that exists.

## Rules of the code

- Pure Rust. No C or assembly is compiled anywhere in the dependency tree, which
  is why `cargo tree -i cc` must print nothing. `clap`, `mysql_async` and
  `sqlparser` have trimmed features in the root `Cargo.toml`; keep them trimmed.
- `unsafe` code is forbidden in the workspace.
- `kilit-core` does no I/O. It takes text and returns data.
- Only `parse.rs` touches sqlparser's types.
- Make the smallest change that does the job.

## Project layout

```text
crates/kilit/        the kilit command
crates/kilit-core/   reads migrations and checks them; no I/O
crates/kilit-lab/    measures schema changes on real MySQL servers
scenarios/           one TOML file per measured change, and the lab's tables
matrix/              the lab's results, one JSON file per server release
docs/                reference pages
compose.yaml         the MySQL servers the lab runs against
```

How the parts work together is in [the architecture notes](docs/architecture.md).

## License of contributions

kilit is licensed under either of
[Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you say otherwise, a
contribution you submit is licensed in the same way, without additional terms
or conditions.
