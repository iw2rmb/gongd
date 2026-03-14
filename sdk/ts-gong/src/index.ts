import { createConnection, type Socket } from "node:net";
import { createInterface } from "node:readline";
export const VERSION = "v0.1.0";
export const DEFAULT_EVENT_SOCKET = "/tmp/gongd.sock";
export const DEFAULT_CONTROL_SOCKET = "/tmp/gongd.ctl.sock";

export type EventType =
  | "file_created"
  | "file_modified"
  | "file_deleted"
  | "file_renamed"
  | "dir_created"
  | "dir_deleted"
  | "dir_renamed"
  | "repo_head_changed"
  | "repo_index_changed"
  | "repo_refs_changed"
  | "repo_packed_refs_changed"
  | "repo_changed";

export interface Event {
  repo: string;
  type: EventType;
  path: string | null;
  git_path: string | null;
  ts_unix_ms: number;
}

export interface ControlResponse {
  ok: boolean;
  message?: string;
  error?: string;
  repos?: string[];
}

export interface ClientOptions {
  eventSocket?: string;
  controlSocket?: string;
}

export interface SubscribeOptions {
  signal?: AbortSignal;
}

type RepoControlOp = "add_watch" | "remove_watch";

const RECONNECT_DELAY_MS = 100;
const RECONNECTABLE_SOCKET_ERRORS = new Set([
  "ENOENT",
  "ECONNREFUSED",
  "ECONNABORTED",
  "ECONNRESET",
  "ENOTCONN",
  "EADDRNOTAVAIL"
]);

export class DaemonError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DaemonError";
  }
}

export class Client {
  readonly eventSocket: string;
  readonly controlSocket: string;

  constructor(options: ClientOptions = {}) {
    this.eventSocket = options.eventSocket ?? DEFAULT_EVENT_SOCKET;
    this.controlSocket = options.controlSocket ?? DEFAULT_CONTROL_SOCKET;
  }

  addWatch(repo: string, signal?: AbortSignal): Promise<ControlResponse> {
    return this.sendRepoControl("add_watch", repo, signal);
  }

  removeWatch(repo: string, signal?: AbortSignal): Promise<ControlResponse> {
    return this.sendRepoControl("remove_watch", repo, signal);
  }

  async listWatches(signal?: AbortSignal): Promise<string[]> {
    const response = await this.sendControl({ op: "list_watches" }, signal);
    return response.repos ?? [];
  }

  async *subscribe(options: SubscribeOptions = {}): AsyncGenerator<Event> {
    const { signal } = options;
    let socket = await connectSocket(this.eventSocket, signal);

    while (true) {
      const rl = createInterface({ input: socket, crlfDelay: Infinity });
      try {
        for await (const line of rl) {
          throwIfAborted(signal);
          const trimmed = line.trim();
          if (trimmed.length === 0) {
            continue;
          }

          yield JSON.parse(trimmed) as Event;
        }
      } catch (error) {
        if (signal?.aborted) {
          throw abortError();
        }
        if (!isReconnectableError(error)) {
          throw toError(error);
        }
      } finally {
        rl.close();
        socket.destroy();
      }

      socket = await reconnectSocket(this.eventSocket, signal);
    }
  }

  private sendRepoControl(
    op: RepoControlOp,
    repo: string,
    signal?: AbortSignal
  ): Promise<ControlResponse> {
    return this.sendControl({ op, repo }, signal);
  }

  private async sendControl(
    request: { op: string; repo?: string },
    signal?: AbortSignal
  ): Promise<ControlResponse> {
    const socket = await connectSocket(this.controlSocket, signal);
    try {
      const linePromise = readOneLine(socket, signal);
      socket.write(`${JSON.stringify(request)}\n`);
      const line = await linePromise;
      const response = JSON.parse(line) as ControlResponse;
      if (!response.ok) {
        throw new DaemonError(
          response.error ?? "gongd returned an unsuccessful response"
        );
      }
      return response;
    } finally {
      socket.destroy();
    }
  }
}

async function reconnectSocket(
  socketPath: string,
  signal?: AbortSignal
): Promise<Socket> {
  while (true) {
    throwIfAborted(signal);
    try {
      return await connectSocket(socketPath, signal);
    } catch (error) {
      if (!isReconnectableError(error)) {
        throw error;
      }
      await delay(RECONNECT_DELAY_MS, signal);
    }
  }
}

async function connectSocket(
  socketPath: string,
  signal?: AbortSignal
): Promise<Socket> {
  throwIfAborted(signal);
  return await new Promise<Socket>((resolve, reject) => {
    const socket = createConnection(socketPath);

    const cleanup = () => {
      socket.off("connect", onConnect);
      socket.off("error", onError);
      signal?.removeEventListener("abort", onAbort);
    };

    const onConnect = () => {
      cleanup();
      resolve(socket);
    };

    const onError = (error: Error) => {
      cleanup();
      socket.destroy();
      reject(toError(error));
    };

    const onAbort = () => {
      cleanup();
      socket.destroy(abortError());
      reject(abortError());
    };

    socket.once("connect", onConnect);
    socket.once("error", onError);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

async function readOneLine(socket: Socket, signal?: AbortSignal): Promise<string> {
  const rl = createInterface({ input: socket, crlfDelay: Infinity });
  const onAbort = () => rl.close();
  signal?.addEventListener("abort", onAbort, { once: true });

  try {
    for await (const line of rl) {
      throwIfAborted(signal);
      const trimmed = line.trim();
      if (trimmed.length > 0) {
        return trimmed;
      }
    }
    throw new Error("control socket closed before response");
  } finally {
    signal?.removeEventListener("abort", onAbort);
    rl.close();
  }
}

function isReconnectableError(error: unknown): boolean {
  if (!(error instanceof Error)) {
    return false;
  }
  const code = (error as NodeJS.ErrnoException).code;
  return typeof code === "string" && RECONNECTABLE_SOCKET_ERRORS.has(code);
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw abortError();
  }
}

function abortError(): Error {
  return new Error("The operation was aborted");
}

function toError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function delay(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const onAbort = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      reject(abortError());
    };

    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);

    signal?.addEventListener("abort", onAbort, { once: true });
  });
}
