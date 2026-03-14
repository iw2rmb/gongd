[![Rust](https://github.com/iw2rmb/gongd/actions/workflows/rust.yml/badge.svg)](https://github.com/iw2rmb/gongd/actions/workflows/rust.yml)

# gongd

`gongd` (git + pong + daemon) is a small Unix-socket daemon for watching git-managed repos and broadcasting events to subscribers like tooling, IDE, LSP.


## What it does

- respects `.gitignore`
- respects `.git/info/exclude`
- respects the global Git ignore file when configured via `core.excludesFile`
- watches one or more repository roots
- persists the watch set in `~/.gong/config.json`
- watches `~/.gong/` and reconciles active repo watches from `config.json`
- broadcasts events over one Unix socket and accepts control commands over another


## Events

Changes in repo:
- `file_created`
- `file_modified`
- `file_deleted`
- `file_renamed`
- `dir_created`
- `dir_deleted`
- `dir_renamed`

Changes in `.git/`
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


## Install

Please refer to [INSTALL.md](INSTALL.md).


## Run

```bash
cargo run -- \
  --socket /tmp/gongd.sock \
  --control-socket /tmp/gongd.ctl.sock \
  /path/to/repo-a \
  /path/to/repo-b
```

Startup repo arguments are optional.

`~/.gong/config.json` is authoritative. On startup, `gongd` loads it, watches `~/.gong/`, and reconciles the active repo watch set whenever `config.json` changes.

Startup repo arguments are only used to seed `~/.gong/config.json` when the file is missing or its `repos` array is empty.

If a configured repo disappears from disk or stops being a valid repo, `gongd` prunes that path from `config.json` and stops watching it.

If `config.json` is deleted, `gongd` stops all repo watches. If the file contains invalid JSON, `gongd` ignores that update and keeps the current active watch set.

Config format:

```json
{
  "repos": [
    "~/work/repo-a",
    "$HOME/work/repo-b"
  ]
}
```

Repo entries accept `~` and environment variables. `gongd` resolves them to absolute repo roots for duplicate detection and active watches, but preserves the first original spelling when it rewrites `config.json`.


## Read the event stream

Check example at [scripts/client.sh](scripts/client.sh).


## Control socket

The control socket is request/response JSON over a separate Unix socket.

Schema: [schemas/gongd.ctl.schema.json](schemas/gongd.ctl.schema.json).

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


## Example output

```json
{"repo":"/work/api","type":"file_modified","path":"src/main.rs","git_path":null,"ts_unix_ms":1710000000000}
{"repo":"/work/api","type":"repo_head_changed","path":null,"git_path":"HEAD","ts_unix_ms":1710000001000}
{"repo":"/work/api","type":"repo_index_changed","path":null,"git_path":"index","ts_unix_ms":1710000002000}
```


## Protocol

Each connected client receives the same broadcast stream.

Schema: [schemas/gongd.schema.json](schemas/gongd.schema.json).

Transport:
- Unix domain socket
- newline-delimited UTF-8 JSON

Rules:
- `path` is only for worktree events
- `git_path` is only for `.git` events
- all paths are relative to the repository root or `.git/` root respectively


## SDKs

- Go: [sdk/go-gong](sdk/go-gong)
- Rust: [sdk/rs-gong](sdk/rs-gong)
- TypeScript: [sdk/ts-gong](sdk/ts-gong)
