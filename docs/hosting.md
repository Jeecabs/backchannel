# Host your own relay

The relay is a Rust Cloudflare Worker with one Durable Object (SQLite storage).

```sh
cargo install worker-build
cd crates/relay
npx wrangler deploy --env=""           # onto your account's *.workers.dev address
```

The empty `--env` picks the settings at the top of `wrangler.toml`. The `[env.production]` block at the bottom is the hosted relay's own domain: to serve yours, change its `pattern` to your domain and run `npx wrangler deploy --env production` instead.

Then open `https://your-domain/new` and claim your org. Anyone who can reach the relay can claim an unused org name, so put it behind access control if you don't want that.

`wrangler.toml` also declares three rate limiters, each counted per client address: 3 org claims a minute, 10 signups or join requests a minute, and 20 invite-code tests a minute. Past the limit the relay answers 429, and the CLI and the pages tell the person to try again in a minute. The numbers are in `wrangler.toml` for you to change, once at the top and once under `[env.production]`, because wrangler environments don't inherit bindings. They are optional: delete a `[[ratelimits]]` block and the relay still works, with no limit on that endpoint. Cloudflare counts approximately and separately in each of its locations, so treat these as a brake on one noisy source, not a wall.

To offer the CLI from your relay, run `scripts/build-dist.sh` before deploying. It cross-compiles the four binaries into `viewer/static/dl` and writes their checksums to `SHA256SUMS` beside them. `/install.sh` fetches both and refuses a download that doesn't match, so rerun the script whenever you rebuild.

A relay speaks for its own address: its install script, invite links and pages all point back at it, so people who install from your relay or open one of your invite links end up on your relay. The commands that have no link to go on (`backchannel create`, `backchannel join --org`, `backchannel join <bare code>`, `backchannel login`) go to the hosted relay unless told otherwise. Pass `--relay https://your-domain`, or set it once:

```sh
export BACKCHANNEL_RELAY=https://your-domain
```

To hand out a CLI that defaults to your relay, build it with `BACKCHANNEL_DEFAULT_RELAY=https://your-domain` set (for example `BACKCHANNEL_DEFAULT_RELAY=https://your-domain scripts/build-dist.sh`).

The relay holds keys for the people who join through it (custodial signing), and you, as its operator, can read everything stored on it. Tell the people you invite. See [Security and trust model](security.md).
