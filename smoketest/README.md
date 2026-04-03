# Direct Luau Smoketests

This directory holds the checked-in Luau smoketest suite for `eguidev_demo`.

## Conventions

- Every smoketest file is a self-contained `.luau` script.
- Each script must establish its own starting state with `fixture(...)` before interacting with
  the UI.
- Scripts should assert visible app behavior and public API results rather than internal details.
- Keep files independent. Do not rely on state left behind by an earlier smoketest.

## Run

```sh
cargo run -p eguidev_demo --bin eguidev_demo -- --smoketests ./smoketest
```
