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
main (upstream v0.45.0-dev)
│
├── alberto/fork-customizations
│   └── Custom version, git hash in --version, this doc
│
├── alberto/hanging-sessions
│   └── Fix for `zellij ls` hanging + PID display + progressive mode
│
├── alberto/zsh-completions
│   └── Dynamic session name completion for zsh
│
├── alberto/scrollback-wrap-fix
│   └── Fix scrollback limits to count wrapped/display lines correctly
│
└── alberto/my-zellij (4-way merge)
    └── Integration branch combining all features + customizations
```

### Branch Descriptions

| Branch | Purpose | Commits |
|--------|---------|:-------:|
| `main` | Tracks upstream zellij (v0.45.0-dev) | — |
| `alberto/fork-customizations` | Version, git hash, docs | 1 |
| `alberto/hanging-sessions` | Session listing fixes | 9 |
| `alberto/zsh-completions` | Zsh autocompletion | 2 |
| `alberto/scrollback-wrap-fix` | Scrollback line counting fix | 5 |
| `alberto/my-zellij` | Combined features | merge |

### Retired Branches

| Branch | Reason | Date |
|--------|--------|------|
| `alberto/tab-rename-fix` | Upstream fixed the same bug in `d45697a7` (refactor: make tab_ids stable) | 2026-02-13 |

### Visual DAG

```
◆  main (upstream v0.45.0-dev, a8372a09)
│
├── ○ alberto/fork-customizations
│   │  - Version: 0.45.0-my-zellij
│   │  - Git hash in `zellij --version`
│   │  - FORK_WORKFLOW.md
│
├── ○ alberto/hanging-sessions (9 commits)
│   │  - Fix zellij ls hanging (socket timeout)
│   │  - Parallelize session checking
│   │  - Add --progressive flag
│   │  - Display session PIDs
│   │  - Cross-platform gating
│   │  - CI test stability
│
├── ○ alberto/zsh-completions (2 commits)
│   │  - Add session name completion
│   │  - Formatting fix
│
├── ○ alberto/scrollback-wrap-fix (5 commits)
│   │  - Fix scrollback to count display lines
│   │  - Wrapped lines now counted correctly
│   │  - Accurate scroll indicator
│   │  - osc8_hyperlinks test compatibility
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
jj bookmark set main -r main@upstream
```

### Step 3: Rebase all feature branches onto new main

Rebase in order of conflict risk (lowest first):

```bash
# Low risk first
jj rebase -s 'roots(old_main..alberto/fork-customizations)' -d main
jj rebase -s 'roots(old_main..alberto/zsh-completions)' -d main

# Medium risk
jj rebase -s 'roots(old_main..alberto/hanging-sessions)' -d main

# Highest risk last
jj rebase -s 'roots(old_main..alberto/scrollback-wrap-fix)' -d main
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
# Create new merge with all branches
jj new alberto/hanging-sessions alberto/zsh-completions alberto/scrollback-wrap-fix alberto/fork-customizations \
  -m "integration: combine all feature branches"
jj bookmark set alberto/my-zellij --allow-backwards

# Abandon the old conflicted integration commits if they exist
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
jj git push --bookmark alberto/scrollback-wrap-fix
jj git push --bookmark alberto/my-zellij

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
jj new alberto/hanging-sessions alberto/zsh-completions alberto/scrollback-wrap-fix alberto/fork-customizations alberto/new-feature \
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

- **Version**: `0.45.0-my-zellij` (in `Cargo.toml`)
- **Git hash in version output**: `build.rs` captures commit hash; `consts.rs` exposes `GIT_HASH`; `cli.rs` shows it in `zellij --version`
- **FORK_WORKFLOW.md**: This documentation file

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

### Install location

The default install location is `~/.cargo/bin/zellij`. After building:

```bash
# Verify installation
zellij --version
# Should show: zellij 0.45.0-my-zellij (abc1234f)
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

### Recurring grid_tests.rs EOF conflict

Every rebase produces a conflict in `zellij-server/src/panes/unit/grid_tests.rs` because both upstream and our scrollback-wrap-fix branch append tests at the end of the file. Resolution is always the same: keep BOTH sets of tests (upstream first, then ours).

### Cargo.lock conflicts

Always take the upstream version of Cargo.lock during conflict resolution, then let `cargo check` regenerate it:

```bash
jj restore --from main Cargo.lock
```

### Cargo.toml version conflicts

The `fork-customizations` branch sets `version = "X.Y.Z-my-zellij"`. After rebase, update this to match the new upstream version prefix.

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

- Upstream: `0.45.0` (development version)
- Integration: `0.45.0-my-zellij`

The `-my-zellij` suffix allows easy identification of which build is running and helps cargo resolve workspace dependencies correctly.

## Rebase History

| Date | Upstream Range | New Commits | Conflicts | Notes |
|------|---------------|:-----------:|-----------|-------|
| 2026-02-13 | b5a3f278 → e887164a | 34 | grid_tests.rs | Dropped tab-rename-fix (upstream fixed same bug) |
| 2026-03-08 | e887164a → 58cb2267 | 48 | sessions.rs, grid.rs, grid_tests.rs | interprocess v2 migration, osc8_hyperlinks param |
| 2026-03-29 | 58cb2267 → 16beceaa | 48 | grid_tests.rs | v0.44.0 release, Windows support, OSC-99, focus-follows-mouse |
| 2026-04-14 | 16beceaa → a8372a09 | 22 | grid_tests.rs, Cargo.toml/lock | v0.44.1 release, layout string flag, scrollback preservation, BCE fix, rustls migration |

---

*Last updated: 2026-04-15*
*Moved fork customizations to dedicated branch; updated to v0.45.0-dev upstream*
