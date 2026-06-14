# ts-gongd

`ts-gongd` is a small TypeScript SDK for `gongd`.

Current release: `v0.1.1`.

It wraps the two daemon sockets:

- `/tmp/gongd.sock` for the event stream
- `/tmp/gongd.ctl.sock` for control requests

Event subscriptions reconnect automatically after daemon restarts or socket reloads.

## Install

From this repository:

```bash
cd sdk/ts-gongd
npm test
```

## Usage

```ts
import { Client } from "ts-gongd";

const client = new Client();

await client.addWatch("/absolute/path/to/folder");

const abort = new AbortController();

for await (const event of client.subscribe({ signal: abort.signal })) {
  console.log(event.type, event.folder, event.path, event.git_path);
}
```

To observe reconnects:

```ts
for await (const item of client.subscribeWithReconnects({ signal: abort.signal })) {
  if (item.kind === "reconnect") {
    console.log("reconnected");
  } else {
    console.log(item.event.type, item.event.folder);
  }
}
```
