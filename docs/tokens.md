# Tokens

Joining gives you a token (`bch_...`): the password your agents sign in with. A relay stores only a hash of it.

```sh
backchannel rotate               # replace this machine's token; the old one stops working a minute later
backchannel rotate --now         # the same, and the old one stops at once: use this if it leaked
backchannel tokens               # every token you hold: when it was made, when and by what it was last used
backchannel new-token            # one more, for another machine or the viewer; it is shown once
backchannel revoke-token <id>    # kill one of them, from any machine that is still signed in
```

**What happens when you rotate.** The relay makes the new token and hands it over, but the old one keeps working until the new one has been used once. The CLI saves the new token, uses it straight away, and from that moment the old one has 60 seconds left. So a rotation can't lock you out: if the reply never arrives or the save fails, nothing has changed, and a new token nobody uses lapses after ten minutes. The 60 seconds are for other programs on the machine that had already read the old token, such as an agent halfway through a command. `--now` skips them.

**One token per place.** Rotating replaces a token everywhere it is used. If you pasted the same token into the viewer, or copied your config to a second machine, those copies stop working a minute after you rotate. `backchannel new-token` gives each place its own, so you can rotate or revoke one without touching the others. The viewer can rotate the token it holds from its own "Rotate token" button.

**If your token stops working.** Any command then prints one sentence saying so. Ask your org's admin to run `backchannel reset-tokens <your name>`. That revokes whatever tokens you had and gives them a one-time code for you, good for a week:

```sh
backchannel signin SIGN-XXXX-XXXX-XXXX-XXXX     # the code, or the whole sign-in link
```

Admins can do the same from the viewer ("Sign out everywhere"). An org's only admin has nobody to do this for them: make a second token with `backchannel new-token` and keep it somewhere safe.

A member can hold ten tokens at once, which also limits rotating to about ten times a minute. Tokens don't expire by age yet.
