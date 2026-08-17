/** The wire revision supported by both the Rust binding and browser client. */
export const PROTOCOL_VERSION = "marksheet-worker@1";
/** Mirrors `SessionLimits::default().max_source_bytes` in the Rust binding. */
export const MAX_SOURCE_BYTES = 5 * 1024 * 1024;
/** Mirrors `MAX_REQUEST_JSON_BYTES` in the Rust binding. */
export const MAX_REQUEST_JSON_BYTES = 32 * 1024 * 1024;
/** Mirrors `MAX_EDIT_OPERATIONS` in the Rust binding. */
export const MAX_EDIT_OPERATIONS = 1024;
const textEncoder = new TextEncoder();

/** @param {Uint8Array} source */
export function assertSourceSize(source) {
  if (source.byteLength > MAX_SOURCE_BYTES) {
    throw new RangeError(`source has ${source.byteLength} bytes, exceeding the ${MAX_SOURCE_BYTES} byte worker limit`);
  }
}

/**
 * Bounds the exact UTF-8 JSON message before the worker hands it to Wasm.
 * This is intentionally a raw-message cap rather than a source-only check:
 * large edit strings, style values, or source expectations are also covered.
 *
 * @param {string} requestJson
 */
export function assertRequestJsonSize(requestJson) {
  const bytes = textEncoder.encode(requestJson).byteLength;
  if (bytes > MAX_REQUEST_JSON_BYTES) {
    throw new RangeError(`request has ${bytes} bytes, exceeding the ${MAX_REQUEST_JSON_BYTES} byte worker JSON limit`);
  }
}

/**
 * Measures the exact JSON size for plain protocol data without constructing
 * its JSON string. This rejects cycles, unsupported JavaScript objects, and
 * oversize traffic before `JSON.stringify` can allocate an unbounded copy.
 * The worker receives structured-clone data, while the public client creates
 * plain objects and arrays, so rejecting exotic objects is intentional.
 *
 * @param {unknown} envelope
 */
export function assertRequestStructureBudget(envelope) {
  if (envelope && typeof envelope === "object" && envelope.request?.kind === "edit"
      && Array.isArray(envelope.request.transaction?.operations)
      && envelope.request.transaction.operations.length > MAX_EDIT_OPERATIONS) {
    throw new RangeError(`edit has more than ${MAX_EDIT_OPERATIONS} operations`);
  }

  let bytes = 0;
  // `active` tracks only the current ancestor chain. A JSON-shaped DAG can
  // legitimately reuse the same Value or StyleProperties object in several
  // edit operations; each occurrence must be measured and serialized. A
  // global seen-set would incorrectly treat that sharing as a cycle.
  /** @type {WeakSet<object>} */
  const active = new WeakSet();
  /** @type {Array<{leave?: object, value?: unknown}>} */
  const stack = [{ value: envelope }];
  const add = (count) => {
    bytes += count;
    if (bytes > MAX_REQUEST_JSON_BYTES) {
      throw new RangeError(`request exceeds the ${MAX_REQUEST_JSON_BYTES} byte worker JSON limit`);
    }
  };
  while (stack.length > 0) {
    const task = stack.pop();
    if (task.leave) {
      active.delete(task.leave);
      continue;
    }
    const value = task.value;
    if (value === null) {
      add(4);
    } else if (typeof value === "string") {
      add(jsonStringByteLength(value));
    } else if (typeof value === "number") {
      add(Number.isFinite(value) ? String(value).length : 4);
    } else if (typeof value === "boolean") {
      add(value ? 4 : 5);
    } else if (Array.isArray(value)) {
      if (active.has(value)) throw new TypeError("worker request cannot contain a cycle");
      active.add(value);
      add(2 + Math.max(0, value.length - 1));
      stack.push({ leave: value });
      for (let index = value.length - 1; index >= 0; index -= 1) stack.push({ value: value[index] });
    } else if (typeof value === "object") {
      if (Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) {
        throw new TypeError("worker request must contain only plain JSON objects");
      }
      if (active.has(value)) throw new TypeError("worker request cannot contain a cycle");
      active.add(value);
      const entries = Object.entries(value);
      add(2 + Math.max(0, entries.length - 1));
      stack.push({ leave: value });
      for (const [key, item] of entries) {
        add(jsonStringByteLength(key) + 1);
        if (item === undefined || typeof item === "function" || typeof item === "symbol" || typeof item === "bigint") {
          throw new TypeError("worker request contains a non-JSON value");
        }
        stack.push({ value: item });
      }
    } else {
      throw new TypeError("worker request contains a non-JSON value");
    }
  }
}

/** @param {string} value */
function jsonStringByteLength(value) {
  let bytes = 2;
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index);
    if (unit === 0x22 || unit === 0x5c) bytes += 2;
    else if (unit <= 0x1f) bytes += 6;
    else if (unit < 0x80) bytes += 1;
    else if (unit < 0x800) bytes += 2;
    else if (unit >= 0xd800 && unit <= 0xdbff && index + 1 < value.length
      && value.charCodeAt(index + 1) >= 0xdc00 && value.charCodeAt(index + 1) <= 0xdfff) {
      bytes += 4;
      index += 1;
    } else if (unit >= 0xd800 && unit <= 0xdfff) bytes += 6;
    else bytes += 3;
  }
  return bytes;
}

/** @param {unknown} value @returns {value is Uint8Array} */
export function isByteArray(value) {
  return value instanceof Uint8Array;
}

/** @param {Uint8Array} left @param {Uint8Array} right */
export function bytesEqual(left, right) {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

/**
 * Applies source-order byte patches against a known source snapshot. The Rust
 * worker guarantees non-overlap and source-relative spans; validating them
 * here prevents a malformed worker reply from corrupting the restart source.
 *
 * @param {Uint8Array} source
 * @param {Array<{span: {start: number, end: number}, replacement: number[]}>} patches
 * @returns {Uint8Array}
 */
export function applySourcePatches(source, patches) {
  let cursor = 0;
  let resultLength = source.byteLength;
  for (const patch of patches) {
    const { start, end } = patch.span;
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < cursor || end < start || end > source.byteLength || !Array.isArray(patch.replacement) || patch.replacement.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
      throw new Error("worker returned invalid source patches");
    }
    resultLength += patch.replacement.length - (end - start);
    if (!Number.isSafeInteger(resultLength) || resultLength < 0) {
      throw new Error("worker returned patches with an invalid result length");
    }
    cursor = end;
  }
  const output = new Uint8Array(resultLength);
  cursor = 0;
  let outputCursor = 0;
  for (const patch of patches) {
    const { start, end } = patch.span;
    output.set(source.subarray(cursor, start), outputCursor);
    outputCursor += start - cursor;
    output.set(patch.replacement, outputCursor);
    outputCursor += patch.replacement.length;
    cursor = end;
  }
  output.set(source.subarray(cursor), outputCursor);
  return output;
}
