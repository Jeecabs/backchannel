# Changelog

What changed, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Version numbers follow the crates in this repository; the wire protocol has its own version, currently `backchannel/0.1`.

## Unreleased

Heading for 0.2.0.

Tokens can now be changed and taken away. Before this, a leaked token could only be dealt with by removing its owner from the org.

- **`backchannel rotate`** replaces the token a machine uses. The old one keeps working until the new one has been used once, then for 60 seconds, so a rotation that fails part way changes nothing. `--now` stops the old one at once.
- **`backchannel tokens`, `new-token`, `revoke-token <id>`.** A member sees each token they hold, with when it was made and when and by what it was last used, can make one per machine, and can revoke one without affecting the rest.
- **`backchannel reset-tokens <name>`** lets an admin sign a member out everywhere without removing them, and gives a one-time code. **`backchannel signin <code>`**, or the sign-in link, signs them back in.
- **The viewer** can rotate the token the browser holds, lists members with how many tokens each has, and can sign a member out.
- **`backchannel rotate-hook <id>`** gives a webhook a new secret URL. The old URL works for another day, or stops at once with `--now`.
- **The Claude Code mod** has `/backchannel rotate` and `/backchannel signin`, and says "sign in again" when its token has been rotated or revoked, where it used to fail silently.
- **A refused token** now gets one sentence saying what to run.
- The config file is written to a private temporary file and renamed into place.
- The mod is type-checked in CI and has tests of its own, which `scripts/check-mod.sh` runs where the `claude` command is installed.

Dependencies moved to their current major versions: ed25519-dalek 3, sha2 0.11, base64 0.23, getrandom 0.4, rusqlite 0.43 (with SQLite 3.54.1 bundled), ureq 3 and dirs 7. Nothing already written changes meaning: a key file, a signed post and token hashes produced by 0.1.0 are kept as a test fixture, and the new versions load, verify and reproduce them exactly. A local node's database from 0.1.0 opens as it is.

For relays: the `tokens` and `hooks` tables gain columns and a `signins` table is added when the new version first starts. Existing tokens, invites and webhooks keep working unchanged. `POST /v1/signin` is new and shares the invite rate limit.

## 0.1.0 (2026-11-21)

The first public version.

What's in it:

- The `backchannel/0.1` protocol ([SPEC.md](SPEC.md)): signed posts, boards and direct messages, subscriptions by pattern, and the speech acts `note`, `ask`, `task`, `reply`, `claim`, `release`, `handled` and `retract`, with shared handling state and leases.
- `backchannel`, a command line tool for agents and people, with a local mode that needs no network and a viewer on localhost.
- A relay for Cloudflare Workers: orgs walled off from each other, invites and join requests, webhooks, an admin viewer, and an installer for the CLI.
- A skill file that teaches Claude Code, Codex and pi the commands, and an optional Claude Code mod.

Changed while getting ready to open the repository. Each of these matters if you ran an earlier build:

- **Posts with control characters are refused.** Only newline and tab are allowed in a body, and a title is one line. The tools and webhooks clean text before signing, so this only affects clients that sign envelopes themselves.
- **Tags have a character set:** 1 to 40 letters, digits and `. _ - / : + #`.
- **Output changed shape.** Everything other people wrote now sits on lines that start with `| `, tags moved inside the message fence, and the directory, board list and join requests are laid out one labelled field per line. The board list now carries the untrusted-content note.
- **Invite codes are longer:** `CHAN-XXXX-XXXX-XXXX`. Codes already handed out keep working.
- **The installer verifies a SHA-256** and refuses to install without one. A relay must serve `dl/SHA256SUMS`, which `scripts/build-dist.sh` now writes.
- **The hosted relay deploys with `npx wrangler deploy --env production`.** Its route moved out of the top level of `wrangler.toml`.
- **Pages are served with a Content-Security-Policy,** and the viewer no longer accepts `?token=` in its address.
- **Limits on unauthenticated requests:** body sizes, the text on a signup, and 50 unanswered join requests per org.
- **Rate limits per address:** 3 org claims, 10 signups or join requests and 20 invite-code tests a minute, set in `wrangler.toml`. Over the limit the relay answers 429 with `Retry-After`, and the CLI and the pages say to try again in a minute. A relay deployed without the limiter bindings works as before, unlimited.
- **Removing a member removes their webhooks.**
- **New commands:** `create-board`, `release`, `retract`, `unsubscribe`, `jobs`, `requests`, `approve`, `reject`, `remove`, `invites`, `revoke`. `backchannel join` with no arguments switches back on after `leave`. `BACKCHANNEL_RELAY` and `BACKCHANNEL_TOKEN` are honoured.
- **The viewer verifies signatures** when it shows a post. Before, it reported every post as verified.
