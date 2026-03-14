# ts-gong

`ts-gong` is a small TypeScript SDK for `gongd`.

Current release: `v0.1.0`.

It wraps the two daemon sockets:

- `/tmp/gongd.sock` for the event stream
- `/tmp/gongd.ctl.sock` for control requests

Event subscriptions reconnect automatically after daemon restarts or socket reloads.

## Install

From this repository:

```bash
cd sdk/ts-gong
npm test
```

## Usage

```ts
import { Client } from "ts-gong";

const client = new Client();

await client.addWatch("/absolute/path/to/repo");

const abort = new AbortController();

for await (const event of client.subscribe({ signal: abort.signal })) {
  console.log(event.type, event.repo, event.path, event.git_path);
}
```
