# Contributing

## Commits

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

Types in use: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`,
`ci`, `chore`. A scope names the module when it helps, e.g. `fix(sched): …`.

A breaking change is marked with `!` after the type/scope (`feat(cli)!: …`)
and a `BREAKING CHANGE:` footer explaining what changed and how to adapt.
Exit codes and flag meanings are part of the contract, so changing either is
breaking.

## Versioning

`trg` uses [Semantic Versioning](https://semver.org/). The version lives in
`Cargo.toml` and each release is tagged `vX.Y.Z`.

**1.0.0 is not scheduled.** It comes when every feature that is going to be
built is in, and the tool has been battle-tested in production. Until then:

- **0.x.0** — bump for each new feature *or* breaking change. Do this freely:
  several minor bumps in one session are fine. Pre-1.0 versions are meant to
  move fast.
- **0.x.y** — bump for bug fixes and small changes that add no feature and
  break nothing.

From 1.0.0 the usual rules apply: major for breaking changes, minor for
features, patch for fixes.

## Changelog

Every user-visible change gets a line under `## [Unreleased]` in
[CHANGELOG.md](CHANGELOG.md), in the same commit as the change. Internal-only
work (refactors, test changes) does not need an entry.

To release:

1. Pick the version from the rules above.
2. Rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, add a fresh,
   empty `## [Unreleased]` above it, and update the compare links at the
   bottom of the file.
3. Set `version` in `Cargo.toml` (and let `Cargo.lock` follow).
4. Commit as `chore(release): X.Y.Z` and tag `vX.Y.Z`.

## Scope

Before proposing a feature, check [OUT-OF-SCOPE.md](OUT-OF-SCOPE.md). Items
under *Out of scope* need a new reason to be reopened; items on the *Wishlist*
are fair game.
