# Security

## Threat model

gitgud opens local repositories you point it at. Treat that like running `git` in the repository yourself:

- Repo-local **hooks run** during merge/push/pull, exactly as they would on the command line.
- Config-based command execution that git would not run for an interactive user is **blocked**: CLI invocations pass `-c protocol.ext.allow=never -c core.fsmonitor=`, so a crafted `.git/config` cannot execute commands via `ext::` remote URLs or a command-valued fsmonitor.
- Paths from the repository's index are confined to the worktree before any filesystem write (conflict resolution).
- Untrusted file content is bounded before processing: diffs are capped at 20,000 lines and lines over 4 KiB skip syntax highlighting.

Local read-only operations (status, history, diff, staging) go through libgit2 and execute no hooks or config commands.

**Do not run fetch/pull/push/merge in repositories you do not trust.**

## Reporting a vulnerability

Open a private report via GitHub's security advisories on this repository ("Security" tab → "Report a vulnerability"). Please include reproduction steps.
