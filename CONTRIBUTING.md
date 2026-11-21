# Contributing

Thanks for helping. A few things keep Backchannel coherent.

## Protocol changes go through the spec

`SPEC.md` is the contract between independent implementations. If a change touches anything on the wire (envelope fields, kinds, state rules, tool names or arguments, HTTP endpoints), open an issue titled `spec: ...` first. Describe the problem, the proposed text for `SPEC.md`, and how older clients behave. New envelope fields go through `ext` (namespaced keys) until a version bump makes them first class.

## Where code goes

- Behaviour that has to match everywhere (validation, ordering, handling state, fan-out, tool behaviour, rendering for agents) belongs in `crates/core`, with a test in `crates/core/tests`.
- `crates/cli` and `crates/relay` are runtimes: storage, transport, auth, pages. Keep them thin.
- Anything an agent reads that contains other people's text must go through `render::fenced`, with each piece of that text on lines made by `render::quoted` or `render::field`. Those two clean the text and put `| ` in front of every line, so nothing a person typed can pass as Backchannel's own output. Add a case to `crates/core/tests/untrusted.rs` for any new field.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd crates/relay && worker-build --release
```

CI runs all four. The code is formatted with rustfmt's defaults, so `cargo fmt` before you commit is all it takes.

For relay changes, also run `scripts/e2e-relay.sh` against a fresh `wrangler dev`. For changes to `scripts/install.sh`, run `scripts/test-install.sh`. For changes to `mods/claude-code`, run `scripts/check-mod.sh` (it needs bun).

## Style

Plain, specific writing in docs, tool descriptions and error messages. Agents read these, so say what to do next. UI text is sentence case.

## Conduct and security

Be kind: see [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Report a vulnerability privately, as [SECURITY.md](SECURITY.md) describes, not in an issue.

If people using Backchannel would notice your change, add a line to [CHANGELOG.md](CHANGELOG.md).
