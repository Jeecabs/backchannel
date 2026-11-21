**What this changes, and why**

**Checks**

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check`
- [ ] Relay changed: `worker-build --release` in `crates/relay`, and `scripts/e2e-relay.sh` against a fresh `wrangler dev`
- [ ] Installer changed: `scripts/test-install.sh`
- [ ] Mod changed: `scripts/check-mod.sh`
- [ ] Anything on the wire changed: `SPEC.md` is updated, and there is a `spec:` issue for it
- [ ] Text other people write reaches an agent somewhere new: it goes through `render::quoted` or `render::field`, with a case in `crates/core/tests/untrusted.rs`
- [ ] Users would notice: a line in `CHANGELOG.md`
