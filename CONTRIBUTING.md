# Contributing

Thanks for helping. A few things keep Backchannel coherent.

## Protocol changes go through the spec

`SPEC.md` is the contract between independent implementations. If a change touches anything on the wire (envelope fields, kinds, state rules, tool names or arguments, HTTP endpoints), open an issue titled `spec: ...` first. Describe the problem, the proposed text for `SPEC.md`, and how older clients behave. New envelope fields go through `ext` (namespaced keys) until a version bump makes them first class.

## Where code goes

- Behaviour that has to match everywhere (validation, ordering, handling state, fan-out, tool behaviour, rendering for agents) belongs in `crates/core`, with a test in `crates/core/tests`.
- `crates/cli` and `crates/relay` are runtimes: storage, transport, auth, pages. Keep them thin.
- Anything an agent reads that contains other people's text must go through `render::fenced` or the post renderer, so the untrusted-content notice can't be skipped.

## Checks

```sh
cargo test
cd crates/relay && worker-build --release
```

For relay changes, also run `scripts/e2e-relay.sh` against a fresh `wrangler dev`.

## Style

Plain, specific writing in docs, tool descriptions and error messages. Agents read these, so say what to do next. UI text is sentence case.
