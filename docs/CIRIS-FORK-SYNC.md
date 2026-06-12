# Keeping the CIRIS fork in sync with upstream

This fork tracks **upstream** `Lew_Palm/leviculum` (codeberg). Our `main` is
`upstream/master` plus a small, **rebasable** patch series — deliberately kept
thin so we inherit upstream's improvements with one command.

## The two layers of our series

1. **Upstream-bound fixes** — opened as PRs against `Lew_Palm/leviculum`
   (cross-platform IPC/RPC, msgpack RPC, thumbv6m/tracing-optional, driver
   robustness, accessors, TCP connect-timeout, …). When upstream merges one,
   `git rebase` drops it from our series automatically. Our goal is for this
   layer to shrink to zero.
2. **Permanent CIRIS-only infra** — never goes upstream:
   - stripped vendored submodules (cargo recursively fetches a git-dependency's
     submodules; a dead ref breaks resolution for consumers like CIRISEdge),
   - GitHub Actions CI (upstream uses codeberg Woodpecker),
   - interop tests resolving RNS via pip when the submodule is absent.

## Syncing to a newer upstream

```sh
scripts/ciris-sync-upstream.sh        # fetch upstream, rebase series, validate
git push --force-with-lease origin main
```

The script rebases our series onto the latest `upstream/master`, drops any of
our commits already merged upstream, and runs the build/test/lint gate
(core+std build & tests, thumbv6m M0 build, fmt, clippy). The full interop
suite additionally needs `pip install rns`.

## Downstream note (CIRISEdge / CIRISLensCore)

These pin a specific rev of this fork and build `reticulum-core`/`reticulum-std`
as a cargo git dependency. After a sync that advances the upstream base, bump
their pin and run their CI — an upstream release can change the public API.
