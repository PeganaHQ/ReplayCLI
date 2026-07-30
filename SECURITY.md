# Security Policy

This repo is the verification layer behind every Pegana alert receipt
(canonical hashing, on-chain anchor checks, methodology replay). A bug here
could let a tampered receipt verify as clean, so we treat reports against it
seriously.

## Reporting a vulnerability

**Preferred — GitHub private vulnerability reporting:**
[open a draft advisory](https://github.com/PeganaHQ/ReplayCLI/security/advisories/new).
It is private until we publish it, gives you an audit trail, and is the route
to a CVE if one is warranted.

**Alternative — Telegram:** [@PeganaWatchBot](https://t.me/PeganaWatchBot).

Either way, please report privately before any public disclosure. Do not open a
public GitHub issue for a suspected vulnerability.

We aim to acknowledge within 72 hours. Pegana is run by a solo maintainer — if
you have not heard back in a week, ping the other channel before assuming the
report was ignored.

Include, if possible:
- The affected crate/file and a minimal reproduction (a receipt, alert ID, or
  input that triggers the issue)
- Whether the bug affects verification correctness (a tampered receipt passes)
  or availability (a crash/panic on adversarial input) — these are triaged
  differently

## Scope

In scope: `crates/methodology`, `crates/pegana-replay-cli`,
`crates/common-verify` — the code in this repository.

Out of scope: the private engine, API, indexer, bot, and web app that produce
the receipts this CLI verifies. Report issues with pegana.xyz itself the same
way, via Telegram.
