# gongd

`gongd` (git + pong + daemon) is a small Unix-socket daemon for local development tooling.

Current release: `0.1.0`.
Versioning follows SemVer.

It watches one or more local Git repositories, filters worktree events using Git-compatible ignore rules, watches `.git/` for repository state changes, and broadcasts newline-delimited JSON events to every connected Unix-socket client.

## What it does

- respects `.gitignore`
- respects `.git/info/exclude`
- respects the global Git ignore file when configured via `core.excludesFile`
- watches one or more repository roots
- persists the watch set in `~/.gong/config.json`
- watches `~/.gong/` and reconciles active repo watches from `config.json`
- broadcasts events over one Unix socket and accepts control commands over another

## Current scope

This implementation is intentionally small and local-first.

It emits:
- `file_created`
- `file_modified`
- `file_deleted`
- `file_renamed`
- `dir_created`
- `dir_deleted`
- `dir_renamed`
- `repo_head_changed`
- `repo_index_changed`
- `repo_refs_changed`
- `repo_packed_refs_changed`
- `repo_changed`

## Limitations

- `.git` must be a directory. Repositories where `.git` is a file that points to another gitdir, such as some worktree setups, are not handled yet.
- The ignore matcher is built at startup. If ignore files change while the daemon is running, restart the daemon.
- Rename reporting depends on the backend watcher and platform behavior.

## Why this shape

A Git-aware watcher is best modeled as two event streams:

1. worktree changes for untracked edits and file creation/deletion
2. `.git/` metadata changes for repository transitions

## Build

```bash
cargo build --release
```

## Install

Install the binary into Cargo's user bin directory:

```bash
cargo install --path .
```

That typically places `gongd` at `~/.cargo/bin/gongd`.

## Install With Homebrew

Tap this repository, then install the formula:

```bash
brew tap iw2rmb/gongd https://github.com/iw2rmb/gongd
brew install gongd
```

## Run

```bash
cargo run -- \
  --socket /tmp/gongd.sock \
  --control-socket /tmp/gongd.ctl.sock \
  /path/to/repo-a \
  /path/to/repo-b
```

If installed with `cargo install`, run:

```bash
~/.cargo/bin/gongd \
  --socket /tmp/gongd.sock \
  --control-socket /tmp/gongd.ctl.sock \
  /absolute/path/to/repo-a \
  /absolute/path/to/repo-b
```

Startup repo arguments are optional.

`~/.gong/config.json` is authoritative. On startup, `gongd` loads it, watches `~/.gong/`, and reconciles the active repo watch set whenever `config.json` changes.

Startup repo arguments are only used to seed `~/.gong/config.json` when the file is missing or its `repos` array is empty.

If `config.json` is deleted, `gongd` stops all repo watches. If the file contains invalid JSON, `gongd` ignores that update and keeps the current active watch set.

Config format:

```json
{
  "repos": [
    "/absolute/path/to/repo-a",
    "/absolute/path/to/repo-b"
  ]
}
```

## Service install

Template units are provided in `deploy/`:

- `deploy/gongd.service` for `systemd --user`
- `deploy/local.gongd.plist` for `launchd`

The templates use:

- event socket: `/tmp/gongd.sock`
- control socket: `/tmp/gongd.ctl.sock`
- config file: `~/.gong/config.json`

They invoke `gongd` directly, so the service environment must have `gongd` on `PATH`. If you install with `cargo install --path .`, ensure `~/.cargo/bin` is visible to `systemd --user` or `launchd`.

If you want fixed startup repos from the service definition, append them to `ExecStart` or `ProgramArguments`. They seed `~/.gong/config.json` only when the file is missing or empty.

### macOS launchd

Copy the template into `~/Library/LaunchAgents/`, then load it:

```bash
cp deploy/local.gongd.plist ~/Library/LaunchAgents/local.gongd.plist
launchctl unload ~/Library/LaunchAgents/local.gongd.plist 2>/dev/null || true
launchctl load ~/Library/LaunchAgents/local.gongd.plist
launchctl start local.gongd
```

### Linux systemd

Copy the template into `~/.config/systemd/user/`, then enable it:

```bash
mkdir -p ~/.config/systemd/user
cp deploy/gongd.service ~/.config/systemd/user/gongd.service
systemctl --user daemon-reload
systemctl --user enable --now gongd
```

## Read the event stream

With `socat`:

```bash
socat - UNIX-CONNECT:/tmp/gongd.sock
```

With `nc` on systems that support Unix sockets:

```bash
nc -U /tmp/gongd.sock
```

## Control socket

The control socket is request/response JSON over a separate Unix socket.

Default path:

```text
/tmp/gongd.ctl.sock
```

List watches:

```bash
printf '%s\n' '{"op":"list_watches"}' | socat - UNIX-CONNECT:/tmp/gongd.ctl.sock
```

Add a watch:

```bash
printf '%s\n' '{"op":"add_watch","repo":"/absolute/path/to/repo"}' | socat - UNIX-CONNECT:/tmp/gongd.ctl.sock
```

Remove a watch:

```bash
printf '%s\n' '{"op":"remove_watch","repo":"/absolute/path/to/repo"}' | socat - UNIX-CONNECT:/tmp/gongd.ctl.sock
```

`add_watch` and `remove_watch` rewrite `~/.gong/config.json`. The config watcher applies the resulting watch-set change.

Machine-readable schema:
- `schemas/gongd.ctl.schema.json`

## Example output

```json
{"repo":"/work/api","type":"file_modified","path":"src/main.rs","git_path":null,"ts_unix_ms":1710000000000}
{"repo":"/work/api","type":"repo_head_changed","path":null,"git_path":"HEAD","ts_unix_ms":1710000001000}
{"repo":"/work/api","type":"repo_index_changed","path":null,"git_path":"index","ts_unix_ms":1710000002000}
```

## Protocol

Each connected client receives the same broadcast stream.

Transport:
- Unix domain socket
- newline-delimited UTF-8 JSON

Rules:
- `path` is only for worktree events
- `git_path` is only for `.git` events
- all paths are relative to the repository root or `.git/` root respectively

Machine-readable schema:
- `schemas/gongd.schema.json`

## Suggested next steps

Good follow-ups for a second version:

- support gitdir files and worktrees
- reload ignore matchers when `.gitignore` changes
- add per-client subscriptions
- add event batching
- add UDP mode
- include commit OID / branch name on `HEAD` changes
- expose a health endpoint

## SDKs

- Go SDK: `sdk/go-gongd`
- Rust SDK: `sdk/rust-gongd`
