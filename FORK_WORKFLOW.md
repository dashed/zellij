# Custom Zellij Fork Workflow

This document describes the branch structure and workflow for maintaining a custom zellij fork with independent feature branches that can be selectively combined.

## Overview

This fork uses a **modular feature branch strategy** where:

- `main` tracks upstream zellij releases
- Each feature lives in its own independent branch based on `main`
- `my-zellij` is a merge commit that combines all desired features
- Jujutsu (jj) is used for version control alongside Git

This approach allows:
- Easy updates when upstream releases new versions
- Selective feature inclusion (enable/disable features by changing the merge)
- Clean separation of concerns
- Simple conflict resolution per-feature

## Branch Structure

```
main (upstream)
│
├── alberto/tab-rename-fix
│   └── Fix for tab rename using position instead of ID
│
├── alberto/hanging-sessions
│   └── Fix for `zellij ls` hanging + PID display + progressive mode
│
├── alberto/zsh-completions
│   └── Dynamic session name completion for zsh
│
└── alberto/my-zellij (3-way merge)
    └── Integration branch combining all features
```

### Branch Descriptions

| Branch | Purpose | Version |
|--------|---------|---------|
| `main` | Tracks upstream zellij | 0.44.0 |
| `alberto/tab-rename-fix` | Tab rename bug fix | 0.44.0-tab-rename-fix |
| `alberto/hanging-sessions` | Session listing fixes | 0.44.0-session-fixes |
| `alberto/zsh-completions` | Zsh autocompletion | (inherits) |
| `alberto/my-zellij` | Combined features | 0.44.0-my-zellij |

### Visual DAG

```
◆  main (upstream: 1296a46a)
│
├── ○ alberto/tab-rename-fix
│   │  - Fix RenameTab position bug
│   │  - Bump version to 0.44.0-tab-rename-fix
│   │  - Rebuilt wasm plugins
│
├── ○ alberto/hanging-sessions
│   │  - Fix zellij ls hanging
│   │  - Parallelize session checking
│   │  - Add --progressive flag
│   │  - Display session PIDs
│   │  - Bump version
│
├── ○ alberto/zsh-completions
│   │  - Add session name completion
│
└── ○ alberto/my-zellij (merge of all above)
       - Version: 0.44.0-my-zellij
       - Rebuilt wasm plugins for this version
```

## Jujutsu (jj) Setup

This repo uses jj in colocated mode, meaning both `jj` and `git` commands work.

### Why jj?

- **Automatic rebasing**: When you update a parent, descendants auto-rebase
- **First-class conflicts**: Conflicts are stored in commits, resolve when convenient
- **Operation log**: Every operation can be undone with `jj undo`
- **Change IDs**: Stable identifiers that survive rebases (unlike git commit hashes)
- **Multi-parent commits**: Native support for merge commits with 3+ parents

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
jj git fetch --remote origin
# or if upstream is a separate remote:
jj git fetch --remote upstream
```

### Step 2: Update main

```bash
jj bookmark set main -r origin/main --allow-backwards
```

### Step 3: Rebase all feature branches onto new main

```bash
# This rebases ALL feature branch roots onto new main
# jj automatically rebases all descendants including my-zellij
jj rebase -s 'roots(::alberto/my-zellij ~ ::main)' -d main
```

### Step 4: Resolve any conflicts

```bash
# Check for conflicts
jj log -r 'conflicts()'

# For each conflicted commit:
jj new <conflicted-change-id>    # Work on top of conflict
# Edit files to resolve
jj squash                         # Move resolution into parent

# For binary conflicts (wasm files):
jj restore --from main path/to/file.wasm
```

### Step 5: Rebuild and push

```bash
# Rebuild plugins for each feature branch that needs it
jj edit alberto/tab-rename-fix
cargo xtask build --release --plugins-only

# Push all updated branches
jj git push --tracked
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

### Step 3: Update version (optional)

If you want a distinct version for this feature:

```bash
# Edit Cargo.toml to set version = "0.44.0-new-feature"
# Edit workspace dependencies versions
# Rebuild plugins
cargo xtask build --release --plugins-only
```

### Step 4: Add to integration branch

```bash
# Recreate my-zellij with the new feature included
jj new alberto/tab-rename-fix alberto/hanging-sessions alberto/zsh-completions alberto/new-feature -m "integration: combine all feature branches"
jj bookmark set alberto/my-zellij -r @ --allow-backwards
```

## The Integration Branch (my-zellij)

`alberto/my-zellij` is a **merge commit with multiple parents**. It combines all feature branches into a single working build.

### How it works

```bash
# Create a merge with multiple parents:
jj new <branch1> <branch2> <branch3> -m "integration message"
```

When any parent branch is updated (e.g., after rebasing onto new upstream), the merge commit is automatically recreated by jj.

### Version resolution

The integration branch uses `0.44.0-my-zellij` as its version. Cargo.toml/Cargo.lock conflicts between feature branches are resolved to use this version.

### Rebuilding after changes

When the integration branch changes, rebuild plugins:

```bash
jj edit alberto/my-zellij
cargo xtask build --release --plugins-only
```

## Building and Installing

### Build commands

```bash
# Build plugins only (fast, ~6 min)
cargo xtask build --release --plugins-only

# Build everything and install
cargo xtask install ~/.cargo/bin/zellij

# Quick run for development
cargo xtask run
```

### Install location

The default install location is `~/.cargo/bin/zellij`. After building:

```bash
# Verify installation
zellij --version
# Should show: zellij 0.44.0-my-zellij
```

### Troubleshooting builds

If the installed binary crashes but `target/release/zellij` works:

```bash
# Remove and re-copy (fixes macOS code signing cache issues)
rm ~/.cargo/bin/zellij
cp target/release/zellij ~/.cargo/bin/zellij
```

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
jj squash                           # Move changes to parent
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
jj git fetch                        # Fetch from remote
jj git push --tracked               # Push tracked bookmarks
jj git push --bookmark <name>       # Push specific bookmark
```

### Undo mistakes

```bash
jj undo                             # Undo last operation
jj op log                           # View operation history
jj op restore <op-id>               # Restore to specific state
```

## Workflow Tips

### Always check status after operations

```bash
jj status && git status
```

### Force snapshot working copy changes

When changes aren't being absorbed into a commit:

```bash
jj new -m "temp"                    # Triggers snapshot
jj abandon @                        # Remove empty commit
```

### Clean up empty commits

```bash
jj abandon <empty-change-id>
```

### View what will be pushed

```bash
jj git push --dry-run --tracked
```

### Resolve conflicts in order

When rebasing creates conflicts in multiple commits:

1. Find all conflicts: `jj log -r 'conflicts()'`
2. Resolve parent commits first
3. Child commits may auto-resolve when parents are fixed

## File Locations

| File | Purpose |
|------|---------|
| `Cargo.toml` | Main manifest with version |
| `Cargo.lock` | Dependency lock file |
| `zellij-utils/assets/plugins/*.wasm` | Compiled plugin binaries |
| `target/release/zellij` | Built executable |

## Version Scheme

- Upstream: `0.44.0`
- Feature branches: `0.44.0-<feature-name>`
- Integration: `0.44.0-my-zellij`

This allows easy identification of which build is running and helps cargo resolve workspace dependencies correctly.

---

*Last updated: 2025-12-21*
