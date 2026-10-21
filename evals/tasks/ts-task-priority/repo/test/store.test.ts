import { test } from "node:test";
import assert from "node:assert/strict";
import { TaskStore } from "../src/store.ts";
import { formatList } from "../src/format.ts";

test("lists open tasks before done ones", () => {
  const store = new TaskStore();
  store.add("Write report");
  store.add("Buy milk");
  store.complete(1);
  assert.equal(formatList(store.list()), "[ ] Buy milk\n[x] Write report");
});

test("complete rejects an unknown id", () => {
  assert.throws(() => new TaskStore().complete(7), RangeError);
});
