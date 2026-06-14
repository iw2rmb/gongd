[![Rust](https://github.com/iw2rmb/gongd/actions/workflows/rust.yml/badge.svg)](https://github.com/iw2rmb/gongd/actions/workflows/rust.yml)

# gongd

`gongd` (git + pong + daemon) is a small Unix-socket daemon for watching local folders and broadcasting events to subscribers like tooling, IDE, LSP.


## What it does

- watches one or more folder roots
- watches file and directory changes under each folder
- enables Git mode when a watched folder has `.git/` as a directory
- in Git mode, respects `.gitignore`, `.git/info/exclude`, and the global Git ignore file when configured via `core.excludesFile`
- in Git mode, watches `.git/` metadata changes
- reconciles active folders by watching own config file
- broadcasts events over one Unix socket and accepts control commands over another


## Events

Folder changes:
- `file_created`
- `file_modified`
- `file_deleted`
- `file_renamed`
- `dir_created`
- `dir_modified`
- `dir_deleted`
- `dir_renamed`

Git metadata changes:
- `git_head_changed`
- `git_index_changed`
- `git_refs_changed`
- `git_packed_refs_changed`
- `git_changed`


## Limitations

- `.git` must be a directory to enable Git mode. Folders where `.git` is a file are watched as plain folders.
- The ignore matcher is built when Git mode is discovered. If ignore files change while the daemon is running, restart or re-add the folder.
- Rename reporting depends on the backend watcher and platform behavior.


## Install

Please refer to [INSTALL.md](INSTALL.md).


## Run

```bash
cargo run -- \
  --socket /tmp/gongd.sock \
  --control-socket /tmp/gongd.ctl.sock \
  /path/to/folder-a \
  /path/to/folder-b
```

Startup folder arguments are optional.

`~/.gong/config.json` is authoritative. On startup, `gongd` loads it, watches `~/.gong/`, and reconciles the active folder watch set whenever `config.json` changes.

Startup folder arguments are only used to seed `~/.gong/config.json` when the file is missing or its `folders` array is empty.

If a configured folder disappears from disk, `gongd` prunes that path from `config.json` and stops watching it. If `.git/` disappears, the folder remains configured.

If `config.json` is deleted, `gongd` stops all folder watches. If the file contains invalid JSON, `gongd` ignores that update and keeps the current active watch set.

Config format:

```json
{
  "folders": [
    "~/work/folder-a",
    "$HOME/work/folder-b"
  ]
}
```

Folder entries accept `~` and environment variables. `gongd` resolves them to absolute folder roots for duplicate detection and active watches, but preserves the first original spelling when it rewrites `config.json`.


## Read the event stream

Check example at [scripts/client.sh](scripts/client.sh).


## Control socket

The control socket is request/response JSON over a separate Unix socket.

Schema: [schemas/gongd.ctl.schema.json](schemas/gongd.ctl.schema.json)

List watches:

```bash
printf '%s\n' '{"op":"list_watches"}' | socat - UNIX-CONNECT:/tmp/gongd.ctl.sock
```

Add a watch:

```bash
printf '%s\n' '{"op":"add_watch","folder":"/absolute/path/to/folder"}' | socat - UNIX-CONNECT:/tmp/gongd.ctl.sock
```

Remove a watch:

```bash
printf '%s\n' '{"op":"remove_watch","folder":"/absolute/path/to/folder"}' | socat - UNIX-CONNECT:/tmp/gongd.ctl.sock
```

`add_watch` and `remove_watch` rewrite `~/.gong/config.json`. The config watcher applies the resulting watch-set change.


## Example output

```json
{"folder":"/work/api","type":"file_modified","path":"src/main.rs","git_path":null,"ts_unix_ms":1710000000000}
{"folder":"/work/api","type":"git_head_changed","path":null,"git_path":"HEAD","ts_unix_ms":1710000001000}
{"folder":"/work/api","type":"git_index_changed","path":null,"git_path":"index","ts_unix_ms":1710000002000}
```


## Protocol

Each connected client receives the same broadcast stream.

Schema: [schemas/gongd.schema.json](schemas/gongd.schema.json)

Transport:
- Unix domain socket
- newline-delimited UTF-8 JSON

Rules:
- `path` is only for worktree events
- `git_path` is only for `.git` events
- all paths are relative to the folder root or `.git/` root respectively


## SDKs

- Go: [sdk/go-gongd](sdk/go-gongd)
- Rust: [sdk/rs-gongd](sdk/rs-gongd)
- TypeScript: [sdk/ts-gongd](sdk/ts-gongd)
