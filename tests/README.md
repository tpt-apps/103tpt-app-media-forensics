# Integration tests

Cross-crate tests that exercise the engine end to end.

Unit tests live beside the code they test, in each crate's `src/` module under
`#[cfg(test)]`. This directory holds only tests that need more than one crate,
or that need real media from `fixtures/`.

## Conventions

- One behaviour per test, named for the behaviour rather than the function
- No network access; the product is offline by design (spec §96)
- Assert on observable output, not internal state
- Fixtures come from `fixtures/`; never generate media at test time in a way
  that depends on an external encoder being installed

## When adding a fixture

See [`fixtures/README.md`](../fixtures/README.md). Every corrupt-media fixture
must document the anomaly it provokes and must be safe to feed to the engine.