import { test } from "node:test";
import assert from "node:assert/strict";
import { mapLimit } from "../src/pool.ts";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

test("keeps the input order even when later calls finish first", async () => {
  const delays = [30, 5, 20, 1, 10];
  const results = await mapLimit(delays, 2, async (ms, i) => {
    await sleep(ms);
    return `${i}:${ms}`;
  });
  assert.deepEqual(results, ["0:30", "1:5", "2:20", "3:1", "4:10"]);
});

test("never runs more than the limit at once", async () => {
  let running = 0;
  let peak = 0;
  await mapLimit([...Array(12).keys()], 3, async () => {
    running++;
    peak = Math.max(peak, running);
    await sleep(3);
    running--;
  });
  assert.equal(peak, 3);
});

test("rejects with the first error after started calls settle, and starts nothing new", async () => {
  const started: number[] = [];
  let settled = 0;
  await assert.rejects(
    mapLimit([0, 1, 2, 3, 4, 5], 2, async (n) => {
      started.push(n);
      await sleep(n === 1 ? 2 : 15);
      settled++;
      if (n === 1) throw new Error("boom 1");
      return n;
    }),
    /boom 1/,
  );
  assert.deepEqual(started.sort(), [0, 1]);
  assert.equal(settled, 2, "waits for the call already running");
});

test("an empty list resolves at once", async () => {
  assert.deepEqual(await mapLimit([], 4, async () => 1), []);
});
