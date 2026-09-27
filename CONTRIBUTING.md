# Contributing to PocketSpot

Every change follows the same path: an issue describes it, a short-lived
branch implements it, a pull request reviews it, and CI checks it before it
reaches `main`. Releases are cut from tagged commits and built by GitHub
Actions.

## Workflow

1. **Issue first.** Open or pick an issue. It states the context, the scope
   as a checklist and the acceptance criteria, and belongs to a milestone
   (see the [roadmap](docs/roadmap.md)).
2. **Branch.** Branch from `main` as `<type>/<issue>-<slug>`, for example
   `feat/12-pairing-screen` or `fix/31-volume-overflow`.
3. **Commit.** Use [Conventional Commits](https://www.conventionalcommits.org/)
   (see below). Keep commits focused; they are squashed on merge.
4. **Pull request.** Open a PR against `main` using the template. Link the
   issue with `Closes #<n>` and describe how the change was tested.
5. **Merge.** Once CI passes and the review is done, squash-merge with a
   Conventional Commit title. The branch is deleted after merging.

`main` is protected: it only changes through pull requests, and its history
stays linear.

## Commit messages

```
<type>(<optional scope>): <summary in the imperative>

<optional body: why, not how>
```

| Type | Use for |
|---|---|
| `feat` | A new user-facing capability |
| `fix` | A bug fix |
| `refactor` | Code change without behaviour change |
| `test` | Tests only |
| `docs` | Documentation only |
| `ci` | CI and release pipeline |
| `chore` | Tooling, dependencies, repository upkeep |

Scopes follow the areas: `playback`, `ui`, `platform`, `ci`. A breaking
change adds `!` after the type (`feat(playback)!: …`) and explains the break
in the body. The squash-merge titles feed the changelog, so write them for a
reader of the release notes.

## Testing

- **Host tests** run everywhere. Code that talks to Spotify is tested
  through the mock playback engine; no account, network or device is needed.
- **CI** runs formatting, lints and tests on Linux arm64 (the Brick's
  architecture) and macOS for every pull request.
- **Device tests** are the only evidence for behaviour on the hardware:
  sound, controls, suspend, battery and running alongside games. Record them
  with a *Device test report* issue for the exact build. A green CI run never
  replaces one.

## Releases

Versions follow [SemVer](https://semver.org/) (pre-1.0: minor versions may
break). A release is cut from `main` by tagging the release commit
`vX.Y.Z`; GitHub Actions builds the packages, checksums and licence notices
and attaches them to the GitHub release. A release is only published once its
milestone is complete and a device test report confirms it.

## Labels and milestones

| Label | Meaning |
|---|---|
| `type:feature`, `type:bug`, `type:chore`, `type:docs` | Kind of work |
| `area:playback`, `area:ui`, `area:platform`, `area:ci` | Part of the system |
| `device-evidence` | Needs or records a test on real hardware |

Milestones are releases, `v0.1` to `v1.1`, described in the
[roadmap](docs/roadmap.md).

## Coding agents

AI coding agents working in this repository follow [AGENTS.md](AGENTS.md),
which applies the same workflow.

## Security

Never post account tokens, credentials, pairing codes or unredacted logs in
issues or pull requests. See [SECURITY.md](SECURITY.md) for reporting
vulnerabilities.
