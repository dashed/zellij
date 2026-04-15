# Custom Zellij Fork Workflow

This document describes the branch structure and workflow for maintaining a custom zellij fork with independent feature branches that can be selectively combined.

## Overview

This fork uses a **modular feature branch strategy** where:

- `main` tracks upstream zellij releases
- Each feature lives in its own independent branch based on `main`
- `my-zellij` is a merge commit that combines all desired features
- `fork-customizations` holds fork-specific additions (version, git hash, docs)
- Jujutsu (jj) is used for version control alongside Git

This approach allows:
- Easy updates when upstream releases new versions
- Selective feature inclusion (enable/disable features by changing the merge)
- Clean separation of concerns
- Simple conflict resolution per-feature

## Branch Structure

```
main (upstream v0.46.0)
│
├── alberto/fork-customizations
│   └── Custom version, git hash in --version, this doc, CLAUDE.md
│
├── alberto/hanging-sessions
│   └── Parallel `zellij ls` + PID display + progressive mode
│
├── alberto/zsh-completions
│   └── Dynamic session name completion for zsh
│
├── alberto/fd-limit-fix
│   └── Raise the server's open-file limit (macOS defaults to 256)
│
└── alberto/my-zellij (4-way merge)
    └── Integration branch combining all features + customizations
```

### Branch Descriptions

| Branch | Purpose | Commits |
|--------|---------|:-------:|
| `main` | Tracks upstream zellij (v0.46.0) | — |
| `alberto/fork-customizations` | Version, git hash, docs, CLAUDE.md | 1 |
| `alberto/hanging-sessions` | Session listing improvements | 8 |
| `alberto/zsh-completions` | Zsh autocompletion | 2 |
| `alberto/fd-limit-fix` | Open-file limit for the server | 1 |
| `alberto/my-zellij` | Combined features | merge |

### Retired Branches

| Branch | Reason | Date |
|--------|--------|------|
| `alberto/tab-rename-fix` | Upstream fixed the same bug in `d45697a7` (refactor: make tab_ids stable) | 2026-02-13 |
| `alberto/scrollback-wrap-fix` | Upstream #5739 (`bfcdf7ac`, grid memory reduction) stores each wrapped display row separately, so the scrollback cap and `SCROLL n/total` counter already count display lines. 6 of our 8 unit tests and the end-to-end counter test passed unmodified on upstream; the other 2 only catch a cosmetic counter overshoot (~10%) when raw-mode programs emit bare `\n` into a full buffer. Last version: `6cb884f6`. | 2026-11-16 |

Retired commits inside a live branch: `fix(sessions): add socket timeout to prevent zellij ls from hanging` (hanging-sessions) was dropped on 2026-11-16 because upstream `fa131c8b` added the same receive timeout (5 s) in `assert_socket`.

### Visual DAG

```
◆  main (upstream v0.46.0, 281060ca)
│
├── ○ alberto/fork-customizations
│   │  - Version: 0.46.0-my-zellij
│   │  - Git hash in `zellij --version`
│   │  - FORK_WORKFLOW.md, CLAUDE.md (model routing)
│
├── ○ alberto/hanging-sessions (8 commits)
│   │  - Parallelize session checking
│   │  - Add --progressive flag
│   │  - Display session PIDs
│   │  - Cross-platform gating
│   │  - CI test stability
│   │  - clap 4 / nix 0.30 adaptations
│
├── ○ alberto/zsh-completions (2 commits)
│   │  - Add session name completion
│   │  - clap_complete 4 injection strings
│
├── ○ alberto/fd-limit-fix (1 commit)
│   │  - Raise RLIMIT_NOFILE soft limit to min(hard, 10240) at server start
│   │  - Restore the original limit for pane programs
│
└── ○ alberto/my-zellij (merge of all above)
       - Includes all features + fork customizations
```

## Jujutsu (jj) Setup

This repo uses jj in colocated mode, meaning both `jj` and `git` commands work.

### Why jj?

- **Automatic rebasing**: When you update a parent, descendants auto-rebase
- **First-class conflicts**: Conflicts are stored in commits, resolve when convenient
- **Operation log**: Every operation can be undone with `jj undo`
- **Change IDs**: Stable identifiers that survive rebases (unlike git commit hashes)
- **Multi-parent commits**: Native support for merge commits with 4+ parents

### Key jj Concepts

```bash
# Bookmarks = Git branches
jj bookmark list                    # List all bookmarks

# Working copy IS a commit
jj status                           # See current state
jj diff                             # See changes in working copy

# Change IDs vs Commit IDs
# - Change ID (e.g., rpxuumvy): stable across rewrites
# - Commit ID (e.g., 6345ec17): changes when commit is modified
```

## Updating from Upstream

When upstream releases a new version:

### Step 1: Fetch upstream changes

```bash
jj git fetch --all-remotes
```

### Step 2: Update main

```bash
jj bookmark set main -r main@author   # `author` is the upstream zellij-org/zellij remote
```

Fetch `origin` too and check `jj log -r 'conflicts() | divergent()'` before starting: the fork's branches may have been rebased from another machine or session since your last pull.

### Step 3: Rebase all feature branches onto new main

Rebase in order of conflict risk (lowest first):

```bash
# Low risk first
jj rebase -s 'roots(old_main..alberto/fork-customizations)' -d main
jj rebase -s 'roots(old_main..alberto/fd-limit-fix)' -d main
jj rebase -s 'roots(old_main..alberto/zsh-completions)' -d main

# Highest risk last (sessions.rs and the CLI change often upstream)
jj rebase -s 'roots(old_main..alberto/hanging-sessions)' -d main
```

Where `old_main` is the previous main commit (use the commit hash).

### Step 4: Resolve any conflicts

```bash
# Check for conflicts
jj log -r 'conflicts()'

# For each conflicted commit:
jj new <conflicted-commit-id>    # Work on top of conflict
# Edit files to resolve
jj squash -u                      # Move resolution into parent, keep parent's message

# For binary conflicts (wasm files, Cargo.lock):
jj restore --from main path/to/file
```

**Tip:** Resolving the earliest conflicted commit often cascades to fix all descendants automatically.

### Step 5: Rebuild integration branch

```bash
# Re-parent the existing merge (keeps its change ID; it should stay empty)
jj rebase -r alberto/my-zellij \
  -d alberto/hanging-sessions -d alberto/zsh-completions -d alberto/fd-limit-fix -d alberto/fork-customizations

# Or create a fresh merge and abandon the old one
jj new alberto/hanging-sessions alberto/zsh-completions alberto/fd-limit-fix alberto/fork-customizations \
  -m "integration: combine all feature branches"
jj bookmark set alberto/my-zellij --allow-backwards
jj abandon <old-integration-commit>
```

### Step 6: Verify and push

```bash
# Verify build compiles
cargo check --no-default-features

# Push all branches
jj git push --bookmark main
jj git push --bookmark alberto/fork-customizations
jj git push --bookmark alberto/zsh-completions
jj git push --bookmark alberto/hanging-sessions
jj git push --bookmark alberto/fd-limit-fix
jj git push --bookmark alberto/my-zellij
# A retired branch: delete the local bookmark, then remove it from origin
jj git push --deleted

# Sync jj to git
jj git export
git checkout alberto/my-zellij
```

## Adding a New Feature

### Step 1: Create feature branch from main

```bash
jj new main -m "feat: description of feature"
jj bookmark create alberto/new-feature
```

### Step 2: Develop the feature

```bash
# Make changes - they're auto-tracked
# When done with a logical unit:
jj new -m "next part of feature"

# Or edit the description:
jj describe -m "better description"
```

### Step 3: Add to integration branch

```bash
# Recreate my-zellij with the new feature included
jj new alberto/hanging-sessions alberto/zsh-completions alberto/fd-limit-fix alberto/fork-customizations alberto/new-feature \
  -m "integration: combine all feature branches"
jj bookmark set alberto/my-zellij --allow-backwards
```

## The Integration Branch (my-zellij)

`alberto/my-zellij` is a **merge commit with multiple parents**. It combines all feature branches into a single working build.

### How it works

```bash
# Create a merge with multiple parents:
jj new <branch1> <branch2> <branch3> <branch4> -m "integration message"
```

When any parent branch is updated (e.g., after rebasing onto new upstream), the merge commit is automatically recreated by jj.

### Fork Customizations Branch

The `alberto/fork-customizations` branch contains additions specific to this fork:

- **Version**: `0.46.0-my-zellij` (in `Cargo.toml`, plus every path-dependency version pin; see Workflow Tips)
- **Git hash in version output**: `build.rs` captures commit hash; `consts.rs` exposes `GIT_HASH`; `cli.rs` shows it in `zellij --version`
- **FORK_WORKFLOW.md**: This documentation file
- **CLAUDE.md**: Model-routing guidance for Claude Code sessions working on this fork

These are on a dedicated branch (not baked into the integration merge) so they survive rebases cleanly.

## Building and Installing

### Build commands

```bash
# Build everything and install
cargo xtask install ~/.cargo/bin/zellij

# Quick run for development
cargo xtask run

# Check compilation without building plugins
cargo check --no-default-features
```

### Built-in plugins

The built-in plugins (session-manager, status bars, configuration, …) are compiled to wasm and baked into the binary from the committed copies in `zellij-utils/assets/plugins/*.wasm`. Upstream doesn't always refresh those copies when it changes plugin code, so they can be older than the plugin source.

- `cargo xtask install` (like `cargo xtask build --release`) rebuilds the plugins into `zellij-utils/assets/plugins` before building the binary, so the installed binary always has current plugins.
- A plain `cargo build --release` bakes in whatever copies are committed. Don't install from it.
- The rebuild leaves the tracked `.wasm` files modified in the working copy. Discard them afterwards so they never get squashed into a fork branch (rebuilt wasm would conflict with upstream on every rebase):

```bash
jj restore zellij-utils/assets/plugins
```

### Install location

The default install location is `~/.cargo/bin/zellij`. After building:

```bash
# Verify installation
zellij --version
# Should show: zellij 0.46.0-my-zellij (abc1234f)
# The hash in parentheses is the git commit used for the build
```

### Troubleshooting builds

If the installed binary crashes but `target/release/zellij` works:

```bash
# Remove and re-copy (fixes macOS code signing cache issues)
rm ~/.cargo/bin/zellij
cp target/release/zellij ~/.cargo/bin/zellij
```

## Upgrading Running Sessions

### Understanding Zellij's Architecture

Zellij uses a **client-server architecture**:

1. When you create a session, a **server process** is spawned and daemonized
2. The server runs independently in memory, listening on a Unix socket
3. When you attach, your **client** connects to the existing server
4. Replacing the binary on disk doesn't affect running processes (standard Unix behavior)

This means **existing sessions don't automatically get new features** when you install a new binary.

### What Uses the New Binary?

| Action | Server Process | Uses New Binary? |
|--------|----------------|------------------|
| `zellij` (new session) | Spawns new | Yes |
| `zellij attach` (running session) | Connects to existing | **No** |
| `zellij attach` (dead session) | Spawns new (resurrect) | Yes |
| Kill + Resurrect | Spawns new | Yes |

### How to Upgrade a Session

To get new features in an existing session:

```bash
# 1. Kill the session (layout is automatically cached)
zellij kill-session <session-name>

# 2. Resurrect by attaching (spawns new server from new binary)
zellij attach <session-name>
```

Or use the session-manager plugin (`Ctrl+o w`) to delete and recreate.

### What Resurrection Preserves

| Preserved | Not Preserved |
|-----------|---------------|
| Tab layout and names | Running processes |
| Pane positions | Scrollback history |
| Working directories | In-flight commands |
| Plugin configurations | Unsaved editor state |

## Common jj Commands

### Navigation

```bash
jj log                              # View commit graph
jj log -r 'trunk()..@'              # Show commits between main and working copy
jj status                           # Current state
jj diff                             # Changes in working copy
```

### Branching

```bash
jj bookmark list                    # List bookmarks
jj bookmark create <name>           # Create at current commit
jj bookmark set <name> -r <rev>     # Move bookmark
jj bookmark set <name> --allow-backwards  # Move bookmark backward
```

### Editing history

```bash
jj edit <change-id>                 # Edit existing commit
jj new                              # Create new commit
jj squash -u                        # Move changes to parent, keep parent's message
jj describe -m "message"            # Change commit message
jj abandon <change-id>              # Remove commit
```

### Rebasing

```bash
jj rebase -d main                   # Rebase current onto main
jj rebase -s <rev> -d <dest>        # Rebase rev and descendants
jj rebase -r <rev> -d <dest>        # Rebase only rev (not descendants)
```

### Syncing with Git

```bash
jj git fetch --all-remotes          # Fetch from all remotes
jj git push --bookmark <name>       # Push specific bookmark
jj git export                       # Export jj state to git refs
git checkout <branch>               # Ensure git is on a named branch
```

### Undo mistakes

```bash
jj undo                             # Undo last operation
jj op log                           # View operation history
jj op restore <op-id>               # Restore to specific state
```

## Workflow Tips

### Resolve conflicts in order

When rebasing creates conflicts in multiple commits:

1. Find all conflicts: `jj log -r 'conflicts()'`
2. Resolve the EARLIEST conflicted commit first
3. Use `jj squash -u` (not just `jj squash`) to keep the parent's message
4. Child commits often auto-resolve when parents are fixed

### Check whether upstream already fixed it

Before resolving a hard conflict, check whether upstream fixed the same problem. That has retired `tab-rename-fix`, `scrollback-wrap-fix` and the hanging-sessions socket-timeout commit. To decide, run our branch's tests against plain upstream in a throwaway workspace:

```bash
jj workspace add --name check -r main /tmp/zellij-check
# copy our tests in, then from /tmp/zellij-check:
CARGO_TARGET_DIR=<repo>/target cargo test --release -p zellij-server --lib -- <test names>
jj abandon 'check@' && jj workspace forget check && rm -rf /tmp/zellij-check
```

To see what each of our commits was meant to do, list its hunks with their enclosing functions (`jj-hunk list --from '<commit>-' --to <commit> --format text`). After fixing compile errors at a branch tip, `jj-hunk absorb --dry-run` shows which earlier commit each fix belongs in. Absorb stops if a descendant is conflicted (for example, the integration merge mid-rebase); `jj squash --from @ --into <commit>` moves the remaining hunks. A fix to a line that upstream wrote last will not route by absorb; squash it explicitly.

### Cargo.lock conflicts

Always take the upstream version of Cargo.lock during conflict resolution, then let `cargo check` regenerate it:

```bash
jj restore --from main Cargo.lock
```

### Cargo.toml version conflicts

The `fork-customizations` branch sets `version = "X.Y.Z-my-zellij"`. After rebase, update this to match the new upstream version prefix.

A semver pre-release such as `0.46.0-my-zellij` does **not** satisfy a `0.46.0` requirement, so every path dependency's `version = "..."` pin must carry the suffix too. Upstream adds new workspace crates with plain pins (for example `zellij-terminal` and `zellij-integration-tests` in the 2026-11-16 rebase). Suffix them all:

```bash
sed -i '' -E '/path = "(\.\.\/)?zellij-[a-z-]+\/?"/ s/version = "0\.46\.0"/version = "0.46.0-my-zellij"/' \
  Cargo.toml */Cargo.toml
jj restore --from main Cargo.lock && cargo metadata --format-version 1 >/dev/null
```

The resulting `Cargo.lock` should differ from upstream only in the `version = "0.46.0-my-zellij"` lines of workspace crates.

### Always sync jj to git after operations

```bash
jj git export
git checkout alberto/my-zellij
git branch -vv  # Verify all branches track origin
```

## File Locations

| File | Purpose |
|------|---------|
| `Cargo.toml` | Main manifest with version |
| `Cargo.lock` | Dependency lock file |
| `FORK_WORKFLOW.md` | This documentation (in fork-customizations branch) |
| `zellij-utils/build.rs` | Git hash capture at build time |
| `zellij-utils/src/consts.rs` | GIT_HASH constant |
| `zellij-utils/src/cli.rs` | Version string with git hash |
| `target/release/zellij` | Built executable |

## Version Scheme

- Upstream: `0.46.0`
- Integration: `0.46.0-my-zellij`

The `-my-zellij` suffix allows easy identification of which build is running and helps cargo resolve workspace dependencies correctly.

## Rebase History

| Date | Upstream Range | New Commits | Conflicts | Notes |
|------|---------------|:-----------:|-----------|-------|
| 2026-02-13 | b5a3f278 → e887164a | 34 | grid_tests.rs | Dropped tab-rename-fix (upstream fixed same bug) |
| 2026-03-08 | e887164a → 58cb2267 | 48 | sessions.rs, grid.rs, grid_tests.rs | interprocess v2 migration, osc8_hyperlinks param |
| 2026-03-29 | 58cb2267 → 16beceaa | 48 | grid_tests.rs | v0.44.0 release, Windows support, OSC-99, focus-follows-mouse |
| 2026-04-14 | 16beceaa → a8372a09 | 22 | grid_tests.rs, Cargo.toml/lock | v0.44.1 release, layout string flag, scrollback preservation, BCE fix, rustls migration |
| 2026-05-01 | a8372a09 → b5e39d54 | 14 | grid_tests.rs | CSI2031 dark/light theme, fullscreen resize, idle CPU fixes, kitty keyboard fix, rustls-webpki upgrade |
| 2026-06-01 | b5e39d54 → e9173cba | 17 | Cargo.toml/lock | PWA web-client support, independent tab sizes, blocking-panes deadlock fix, host query stall fix, session-manager Esc close fix, plugin permission tightening (entry reconstructed 2026-07-04) |
| 2026-08-23 | e9173cba → bf8d23a4 | 66 | not recorded | v0.45.0 release, Kitty image protocol, clap 4 / nix 0.30 / vte 0.15 upgrades; API breaks fixed in follow-up commits on hanging-sessions, zsh-completions and scrollback-wrap-fix (entry reconstructed 2026-11-16) |
| 2026-11-16 | bf8d23a4 → 281060ca | 132 | Cargo.toml/lock, sessions.rs, commands.rs, main.rs, setup.rs, grid.rs, grid_tests.rs | v0.46.0 release, grapheme clusters, grid memory reduction (#5739), CLI dispatch moved to `src/lib.rs`, attach logic moved to `zellij-client/src/session_resolution.rs`, session sockets named by short ID, new `zellij-terminal` crate. Retired scrollback-wrap-fix (superseded by #5739) and the socket-timeout commit (superseded by `fa131c8b`). Added fd-limit-fix |

---

*Last updated: 2026-11-16*
