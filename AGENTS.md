# AGENTS.md

## Layout

- `src/main.rs`: commands (`list`, `add`, `switch`, `rm`).
- `src/claude.rs`: where Claude Code keeps an account (Keychain service name, `.claude.json`).
- `src/keychain.rs`: Keychain access through `/usr/bin/security`.
- `src/lock.rs`: Claude Code's `proper-lockfile` directory locks.

## Rules

- Call `/usr/bin/security`, not Security.framework. An item created by another binary prompts for access.
- Copy only `claudeAiOauth` between Keychain items. The same item holds `mcpOAuth`, which belongs to the profile.
- Hash the exact `CLAUDE_CONFIG_DIR` string that `add` exports. Never resolve or canonicalize it.
- Take the locks in Claude Code's order: `.oauth_refresh.lock`, `~/.claude.lock`, `~/.claude.json.lock`.
- Never run `claude auth logout` for an account. It can revoke the refresh token that ccs saved.

## Verify

Unit tests do not touch the Keychain. Before a release, run on a Mac with 2 accounts added:

```sh
ccs <a> && claude auth status --json   # email of <a>
ccs <b> && claude auth status --json   # email of <b>
```

Check that `mcpOAuth` in the default item keeps its entries after each switch.

## Commits

Conventional Commits. Release Please opens the version PR.
