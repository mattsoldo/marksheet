import assert from "node:assert/strict";
import test from "node:test";

import {
  MarksheetWorkerClient,
  WorkerCancelledError,
} from "../client.js";
import { saveWithExternalChangeGuard } from "../file-save.js";
import { PROTOCOL_VERSION, applySourcePatches } from "../protocol.js";

class FakeWorker {
  constructor() {
    this.sent = [];
    this.terminated = false;
    this.onmessage = null;
    this.onerror = null;
  }

  postMessage(message) {
    this.sent.push(message);
  }

  terminate() {
    this.terminated = true;
  }

  respond(response) {
    this.onmessage?.({ data: response });
  }
}

function response(request, revision, payload) {
  return {
    protocol: PROTOCOL_VERSION,
    request_id: request.request_id,
    revision,
    response: payload,
  };
}

test("suppresses stale replies without resolving a newer request", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const opening = client.open(Uint8Array.from([1, 2, 3]));
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const snapshot = client.snapshot();
  let settled = false;
  snapshot.finally(() => {
    settled = true;
  });
  worker.respond({
    protocol: PROTOCOL_VERSION,
    request_id: "old-request",
    revision: 0,
    response: { kind: "snapshot", snapshot: {} },
  });
  await Promise.resolve();
  assert.equal(settled, false);

  worker.respond(response(worker.sent[1], 1, { kind: "snapshot", snapshot: {} }));
  await snapshot;
  assert.equal(client.currentRevision, 1);
});

test("a stale success reply rejects promptly then reopens the accepted source", async () => {
  const workers = [];
  const client = new MarksheetWorkerClient(() => {
    const worker = new FakeWorker();
    workers.push(worker);
    return worker;
  });
  const opening = client.open(Uint8Array.from([65, 66, 67]));
  workers[0].respond(response(workers[0].sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const pending = client.snapshot();
  workers[0].respond(response(workers[0].sent[1], 0, { kind: "snapshot", snapshot: {} }));
  await assert.rejects(pending, /stale success response/);
  assert.equal(workers[0].terminated, true);
  assert.equal(workers.length, 2);
  assert.equal(workers[1].sent[0].request.kind, "open");
  assert.deepEqual(workers[1].sent[0].request.source, [65, 66, 67]);
  workers[1].respond(response(workers[1].sent[0], 1, { kind: "opened", snapshot: {} }));
  await Promise.resolve();
  assert.equal(client.currentRevision, 1);
});

test("restart terminates pending work then reopens the exact accepted bytes", async () => {
  const workers = [];
  const client = new MarksheetWorkerClient(() => {
    const worker = new FakeWorker();
    workers.push(worker);
    return worker;
  });
  const source = Uint8Array.from([65, 66, 67]);
  const opening = client.open(source);
  workers[0].respond(response(workers[0].sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const pending = client.snapshot();
  const restart = client.cancelAndRestart();
  await assert.rejects(pending, WorkerCancelledError);
  assert.equal(workers[0].terminated, true);
  assert.equal(workers.length, 2);
  assert.deepEqual(workers[1].sent[0].request.source, [65, 66, 67]);
  workers[1].respond(response(workers[1].sent[0], 1, { kind: "opened", snapshot: {} }));
  await restart;
  assert.equal(client.currentRevision, 1);
});

test("opening a second document uses replace_source at the current revision", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const first = client.open(Uint8Array.from([65]));
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await first;

  const second = client.open(Uint8Array.from([66]));
  assert.equal(worker.sent[1].request.kind, "replace_source");
  assert.equal(worker.sent[1].revision, 1);
  worker.respond(response(worker.sent[1], 2, { kind: "replaced", snapshot: {} }));
  await second;
  assert.equal(client.currentRevision, 2);
  assert.deepEqual([...client.lastAcceptedSource], [66]);
});

test("clones caller bytes and correlates a low-revision worker error", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const source = Uint8Array.from([65, 66]);
  const opening = client.open(source);
  source[0] = 88;
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;
  assert.deepEqual([...client.lastAcceptedSource], [65, 66]);

  const pending = client.snapshot();
  worker.respond(response(worker.sent[1], 0, {
    kind: "error",
    error: { code: "session", message: "Wasm initialization failed", diagnostics: [], diagnostics_omitted: 0 },
  }));
  await assert.rejects(pending, (error) => {
    assert.match(error.message, /Wasm initialization failed/);
    assert.equal(error.diagnostics_omitted, 0);
    return true;
  });
  assert.equal(client.currentRevision, 1);
});

test("a malformed matching error rejects promptly and reopens the accepted source", async () => {
  const workers = [];
  const client = new MarksheetWorkerClient(() => {
    const worker = new FakeWorker();
    workers.push(worker);
    return worker;
  });
  const opening = client.open(Uint8Array.from([65, 66]));
  workers[0].respond(response(workers[0].sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const pending = client.snapshot();
  workers[0].respond(response(workers[0].sent[1], 1, {
    kind: "error",
    error: { code: "session", message: 17, diagnostics: [], diagnostics_omitted: 0 },
  }));
  await assert.rejects(pending, /invalid protocol response/);
  assert.equal(workers[0].terminated, true);
  assert.equal(workers.length, 2);
  assert.equal(workers[1].sent[0].request.kind, "open");
  assert.deepEqual(workers[1].sent[0].request.source, [65, 66]);
  workers[1].respond(response(workers[1].sent[0], 1, { kind: "opened", snapshot: {} }));
  await Promise.resolve();
  assert.equal(client.currentRevision, 1);
});

test("same-length external changes refuse saving and return a rebase handoff", async () => {
  let writes = 0;
  const result = await saveWithExternalChangeGuard({
    expectedBase: Uint8Array.from([65, 66, 67]),
    proposedSource: Uint8Array.from([65, 66, 68]),
    readCurrentBytes: async () => Uint8Array.from([88, 66, 67]),
    writeBytes: async () => {
      writes += 1;
    },
  });
  assert.equal(result.kind, "external_drift");
  assert.equal(writes, 0);
  assert.deepEqual([...result.currentSource], [88, 66, 67]);
});

test("unchanged save does not call the writer", async () => {
  let writes = 0;
  const source = Uint8Array.from([65, 66, 67]);
  const result = await saveWithExternalChangeGuard({
    expectedBase: source,
    proposedSource: source.slice(),
    readCurrentBytes: async () => source.slice(),
    writeBytes: async () => {
      writes += 1;
    },
  });
  assert.equal(result.kind, "unchanged");
  assert.equal(writes, 0);
});

test("large patch reconstruction avoids spread-argument limits", () => {
  const source = new Uint8Array(300_000).fill(65);
  const replacement = new Array(300_000).fill(66);
  const result = applySourcePatches(source, [{ span: { start: 0, end: 0 }, replacement }]);
  assert.equal(result.byteLength, 600_000);
  assert.equal(result[0], 66);
  assert.equal(result[299_999], 66);
  assert.equal(result[300_000], 65);
});

test("large source open serializes without spread-argument limits", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const source = new Uint8Array(300_000).fill(65);
  const opening = client.open(source);
  assert.equal(worker.sent[0].request.source.length, 300_000);
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;
});

test("oversized source is rejected before it is copied into a worker message", () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  assert.throws(() => client.open(new Uint8Array(5 * 1024 * 1024 + 1)), RangeError);
  assert.equal(worker.sent.length, 0);
});

test("an oversized edit payload rejects before posting and cannot leave a pending promise", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const opening = client.open(Uint8Array.from([65]));
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const oversized = "x".repeat(33 * 1024 * 1024);
  const pending = client.edit({ operations: [{ kind: "rename_sheet_label", sheet: "s", label: oversized }] });
  await assert.rejects(pending, RangeError);
  assert.equal(worker.sent.length, 1);
});

test("too many edit operations are rejected before stringification or posting", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const opening = client.open(Uint8Array.from([65]));
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const pending = client.edit({
    operations: Array.from({ length: 1025 }, () => ({ kind: "rename_sheet_label", sheet: "s", label: "S" })),
  });
  await assert.rejects(pending, /more than 1024 operations/);
  assert.equal(worker.sent.length, 1);
});

test("shared Value objects are measured per occurrence and still post", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const opening = client.open(Uint8Array.from([65]));
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const value = { kind: "number", value: 12.5 };
  const pending = client.edit({
    operations: [{ kind: "append_table_row", table: "ledger", fields: [value, value] }],
  });
  assert.equal(worker.sent.length, 2);
  client.dispose();
  await assert.rejects(pending, WorkerCancelledError);
});

test("shared StyleProperties objects are measured per occurrence and still post", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const opening = client.open(Uint8Array.from([65]));
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const properties = { bold: true, italic: false };
  const pending = client.edit({
    operations: [
      { kind: "define_style", style: "one", properties },
      { kind: "define_style", style: "two", properties },
    ],
  });
  assert.equal(worker.sent.length, 2);
  client.dispose();
  await assert.rejects(pending, WorkerCancelledError);
});

test("an actual object cycle is rejected before stringification or posting", async () => {
  const worker = new FakeWorker();
  const client = new MarksheetWorkerClient(() => worker);
  const opening = client.open(Uint8Array.from([65]));
  worker.respond(response(worker.sent[0], 1, { kind: "opened", snapshot: {} }));
  await opening;

  const transaction = { operations: [] };
  transaction.self = transaction;
  await assert.rejects(client.edit(transaction), /cycle/);
  assert.equal(worker.sent.length, 1);
});
