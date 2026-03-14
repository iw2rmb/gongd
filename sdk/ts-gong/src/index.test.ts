import test from "node:test";
import assert from "node:assert/strict";
import { rm } from "node:fs/promises";
import { join } from "node:path";
import { createServer, type Server, type Socket } from "node:net";
import { once } from "node:events";

import { Client, DaemonError, VERSION } from "./index.js";

test("version matches release tag", () => {
  assert.equal(VERSION, "v0.1.0");
});

test("addWatch sends expected request", async () => {
  const socketPath = await socketPathFor("ctl-add");
  const server = createServer();
  try {
    const requestSeen = new Promise<void>((resolve, reject) => {
      server.once("connection", (socket) => {
        collectOneLine(socket)
          .then((line) => {
            const request = JSON.parse(line) as { op: string; repo: string };
            assert.equal(request.op, "add_watch");
            assert.equal(request.repo, "/tmp/repo");
            socket.end('{"ok":true,"message":"watch added"}\n');
            resolve();
          })
          .catch(reject);
      });
    });

    await listen(server, socketPath);
    const client = new Client({ controlSocket: socketPath });
    const response = await client.addWatch("/tmp/repo");

    assert.equal(response.ok, true);
    assert.equal(response.message, "watch added");
    await requestSeen;
  } finally {
    await closeServer(server);
    await cleanupSocket(socketPath);
  }
});

test("listWatches returns repo list", async () => {
  const socketPath = await socketPathFor("ctl-list");
  const server = createServer((socket) => {
    void collectOneLine(socket).then((line) => {
      const request = JSON.parse(line) as { op: string };
      assert.equal(request.op, "list_watches");
      socket.end('{"ok":true,"repos":["/tmp/a","/tmp/b"]}\n');
    });
  });

  try {
    await listen(server, socketPath);
    const client = new Client({ controlSocket: socketPath });
    const repos = await client.listWatches();
    assert.deepEqual(repos, ["/tmp/a", "/tmp/b"]);
  } finally {
    await closeServer(server);
    await cleanupSocket(socketPath);
  }
});

test("removeWatch surfaces daemon error", async () => {
  const socketPath = await socketPathFor("ctl-remove");
  const server = createServer((socket) => {
    void collectOneLine(socket).then((line) => {
      const request = JSON.parse(line) as { op: string; repo: string };
      assert.equal(request.op, "remove_watch");
      assert.equal(request.repo, "/tmp/repo");
      socket.end('{"ok":false,"error":"watch not found"}\n');
    });
  });

  try {
    await listen(server, socketPath);
    const client = new Client({ controlSocket: socketPath });
    await assert.rejects(
      () => client.removeWatch("/tmp/repo"),
      (error: unknown) =>
        error instanceof DaemonError && error.message === "watch not found"
    );
  } finally {
    await closeServer(server);
    await cleanupSocket(socketPath);
  }
});

test("subscribe reconnects after stream EOF", async () => {
  const socketPath = await socketPathFor("events");
  let listener = createServer();
  const ready = deferred<void>();
  const reloaded = deferred<void>();

  const serving = (async () => {
    await listen(listener, socketPath);
    listener.once("connection", (socket) => {
      socket.end(
        '{"repo":"/tmp/repo","type":"file_modified","path":"main.go","git_path":null,"ts_unix_ms":1}\n'
      );
      ready.resolve();
    });

    await ready.promise;
    await closeServer(listener);
    await cleanupSocket(socketPath);

    listener = createServer((socket) => {
      socket.end(
        '{"repo":"/tmp/repo","type":"repo_head_changed","path":null,"git_path":"HEAD","ts_unix_ms":2}\n'
      );
    });
    await listen(listener, socketPath);
    reloaded.resolve();
  })();

  try {
    const client = new Client({ eventSocket: socketPath });
    const subscription = client.subscribe();

    const first = await subscription.next();
    assert.equal(first.done, false);
    if (first.done) {
      throw new Error("expected first event");
    }
    assert.equal(first.value.type, "file_modified");
    assert.equal(first.value.path, "main.go");

    await reloaded.promise;
    const second = await subscription.next();
    assert.equal(second.done, false);
    if (second.done) {
      throw new Error("expected second event");
    }
    assert.equal(second.value.type, "repo_head_changed");
    assert.equal(second.value.git_path, "HEAD");

    await subscription.return(undefined);
    await serving;
  } finally {
    await closeServer(listener);
    await cleanupSocket(socketPath);
  }
});

async function socketPathFor(name: string): Promise<string> {
  const unique = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  return join("/tmp", `ts-gong-${name}-${unique}.sock`);
}

function collectOneLine(socket: Socket): Promise<string> {
  return new Promise((resolve, reject) => {
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      const newline = buffer.indexOf("\n");
      if (newline >= 0) {
        resolve(buffer.slice(0, newline));
      }
    });
    socket.once("error", reject);
    socket.once("end", () => {
      reject(new Error("socket closed before newline"));
    });
  });
}

async function listen(server: Server, socketPath: string): Promise<void> {
  server.listen(socketPath);
  await once(server, "listening");
}

async function closeServer(server: Server): Promise<void> {
  if (!server.listening) {
    return;
  }
  server.close();
  await once(server, "close");
}

async function cleanupSocket(socketPath: string): Promise<void> {
  await rm(socketPath, { force: true });
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
