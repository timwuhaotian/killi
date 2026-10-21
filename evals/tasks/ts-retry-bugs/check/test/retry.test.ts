import { test } from "node:test";
import assert from "node:assert/strict";
import { retry } from "../src/retry.ts";

function flaky(failures: number) {
  let calls = 0;
  const fn = async (attempt: number) => {
    calls += 1;
    if (calls <= failures) throw new Error(`fail ${attempt}`);
    return `ok on ${attempt}`;
  };
  return { fn, calls: () => calls };
}

test("retries async failures until success", async () => {
  const waits: number[] = [];
  const { fn, calls } = flaky(2);
  const result = await retry(fn, { attempts: 3, delayMs: 10, sleep: async (ms) => void waits.push(ms) });
  assert.equal(result, "ok on 3");
  assert.equal(calls(), 3);
  assert.deepEqual(waits, [10, 20]);
});

test("throws the last error after the final attempt", async () => {
  const waits: number[] = [];
  const { fn, calls } = flaky(10);
  await assert.rejects(
    retry(fn, { attempts: 4, delayMs: 5, factor: 3, sleep: async (ms) => void waits.push(ms) }),
    /fail 4/,
  );
  assert.equal(calls(), 4);
  assert.deepEqual(waits, [5, 15, 45]);
});

test("stops at once when shouldRetry says no", async () => {
  const { fn, calls } = flaky(5);
  await assert.rejects(
    retry(fn, { attempts: 5, delayMs: 1, shouldRetry: () => false, sleep: async () => {} }),
    /fail 1/,
  );
  assert.equal(calls(), 1);
});

test("a single attempt still runs once", async () => {
  const { fn } = flaky(0);
  assert.equal(await retry(fn, { attempts: 1, delayMs: 1 }), "ok on 1");
});
