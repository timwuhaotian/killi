import { test } from "node:test";
import assert from "node:assert/strict";
import { TaskStore } from "../src/store.ts";
import { formatList, formatTask } from "../src/format.ts";

test("priority defaults to normal", () => {
  const task = new TaskStore().add("Read");
  assert.equal(task.priority, "normal");
  assert.equal(formatTask(task), "[ ] Read");
});

test("list orders by done, then priority, then id", () => {
  const store = new TaskStore();
  store.add("Water plants", { priority: "low" });
  store.add("Pay rent", { priority: "high" });
  store.add("Email Ana");
  store.add("File taxes", { priority: "high" });
  store.complete(2);
  assert.equal(
    formatList(store.list()),
    ["[ ] ! File taxes", "[ ] Email Ana", "[ ] (low) Water plants", "[x] ! Pay rent"].join("\n"),
  );
});

test("setPriority changes the order", () => {
  const store = new TaskStore();
  store.add("A");
  store.add("B");
  store.setPriority(2, "high");
  assert.deepEqual(store.list().map((t) => t.title), ["B", "A"]);
  assert.throws(() => store.setPriority(9, "low"), RangeError);
});
