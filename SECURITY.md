# Security Policy

`cc-loadout` reads and writes **Claude Code login credentials**. That is the
tool's whole purpose, so this policy spells out the sensitive surface — to help
you tell "working as designed" from a genuine vulnerability — and how to report
a problem privately.

## Sensitive surface

`cc-loadout` reads, snapshots, and atomically rewrites these files (all written
with `0600` permissions):

- `~/.claude/.credentials.json`
- the `oauthAccount` block of `~/.claude.json`
- account snapshots under `~/.local/share/cc-loadout/accounts/<alias>/`

(`$CLAUDE_CONFIG_DIR` is honoured, so the first two may live elsewhere.) These
are Claude Code's own internal files; `cc-loadout` never transmits their
contents anywhere — every operation is local to your machine.

## Supported versions

This is a pre-1.0, single-maintainer project. Only the **latest released
version** receives security fixes; older versions are not patched.

| Version | Supported |
|---|---|
| Latest `0.1.x` release | ✅ |
| Anything older | ❌ |

If you are on an older build, upgrade first (`git pull && ./install.sh`, or
re-run the published installer) before reporting.

## Reporting a vulnerability

**Do not open a public issue for a security problem.** Use GitHub's private
reporting instead: [**Report a vulnerability**](https://github.com/xbluesky/cc-loadout/security/advisories/new)
(Security → Advisories → Report a vulnerability). That keeps the report private
between you and the maintainer until a fix ships.

If private reporting is unavailable to you, email
**xbluesky@users.noreply.github.com** instead.

When you report, please include:

- the `cc-loadout --version` you are running and your OS;
- what you observed and why you believe it is a security issue;
- minimal reproduction steps.

**Never paste credentials, tokens, or the contents of the files listed above
into a report.** Redact any secret before sending. A description of the
mishandling is enough — we do not need your actual credentials to reproduce it.

## Response expectations

Reports are handled on a **best-effort basis with no guaranteed response time
(no SLA)** — this is a personal tool maintained in spare time. You will get a
reply when the maintainer is able to triage it. Critical credential-exposure
issues are prioritised over everything else.
