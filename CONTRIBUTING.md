# Contributing to cc-loadout

Thanks for taking the time to contribute! `cc-loadout` is a small, single-binary
Rust CLI, so the contribution loop is deliberately lightweight.

## Ways to contribute

- **Report a bug** — open an issue using the **Bug** template.
- **Request a feature** — open an issue using the **Feature** template.
- **Send a change** — open a pull request (see below).

Before filing, a quick search of existing issues saves everyone a round-trip.

## Development setup

You need a Rust toolchain. The pinned version lives in
[`rust-toolchain.toml`](rust-toolchain.toml) (`rust-version = 1.80` or newer);
`rustup` will install it automatically on first build.

```bash
git clone https://github.com/xbluesky/cc-loadout
cd cc-loadout
cargo build
```

## The check gate

A change is ready for review when **all** of these pass locally — they are the
same checks CI (`.github/workflows/ci.yml`) runs, so green locally means green in CI:

```bash
cargo fmt --check                                       # formatting
cargo clippy --all-targets --all-features -- -D warnings  # lint (warnings are errors)
cargo test                                              # unit + integration (tests/cli.rs)
cargo audit                                             # dependency advisories (.cargo/audit.toml)
./tests/run.sh                                          # bash installer/registry tests (needs bash, jq, git)
```

If `cargo fmt --check` reports diffs, run `cargo fmt` and re-stage. Don't skip
formatting because tests passed — `cargo test` does not invoke rustfmt and
clippy does not enforce formatting.

## Commit messages — Conventional Commits (required)

Commit subjects **must** follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<optional scope>): <imperative summary>
```

Allowed types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`, `perf`.

This is a **hard requirement, not a style preference**: releases are automated
with [`release-plz`](release-plz.toml), which derives the next version number
and the `CHANGELOG.md` sections directly from commit types. A `feat:` bumps the
minor version, a `fix:` the patch version; an un-typed or mis-typed subject
silently breaks the release automation. Keep the subject in the imperative mood,
under ~50 characters, with no trailing period.

## Sign your commits (DCO)

This project uses the [Developer Certificate of Origin](https://developercertificate.org/).
Every commit must carry a `Signed-off-by` line certifying that you wrote the
change (or have the right to submit it). Add it automatically with:

```bash
git commit -s -m "fix(profile): ..."
```

which appends:

```
Signed-off-by: Your Name <your.email@example.com>
```

Use a real name and a reachable email. Commits without a sign-off will be asked
to amend.

## Opening a pull request

1. Fork the repo and branch off `master`.
2. Make your change; keep commits focused and conventionally typed.
3. Push and open a pull request — fill in the pull request template.
4. Make sure the Actions run is green; CI runs the full check gate above.
5. A maintainer reviews, may request changes, then merges.

Small, self-contained PRs get reviewed fastest. If a change is large, consider
opening an issue first to align on the approach.

## Code of Conduct

Participation is governed by our [Code of Conduct](CODE_OF_CONDUCT.md). By
contributing you agree to uphold it.

## Security issues

Found a security problem? **Do not open a public issue.** See
[SECURITY.md](SECURITY.md) for how to report it privately and what to redact.
