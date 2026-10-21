import { test } from "node:test";
import assert from "node:assert/strict";
import { LruCache } from "../src/lru.ts";

test("evicts the least recently used key", () => {
  const cache = new LruCache<string, number>(2);
  cache.set("a", 1);
  cache.set("b", 2);
  cache.set("c", 3);
  assert.equal(cache.has("a"), false);
  assert.deepEqual(cache.keys(), ["b", "c"]);
});

test("get marks a key as recently used", () => {
  const cache = new LruCache<string, number>(2);
  cache.set("a", 1);
  cache.set("b", 2);
  assert.equal(cache.get("a"), 1);
  cache.set("c", 3);
  assert.deepEqual(cache.keys(), ["a", "c"]);
  assert.equal(cache.get("b"), undefined);
});

test("updating a key never evicts another", () => {
  const cache = new LruCache<string, number>(2);
  cache.set("a", 1);
  cache.set("b", 2);
  cache.set("a", 10);
  assert.equal(cache.size, 2);
  assert.deepEqual(cache.keys(), ["b", "a"]);
  assert.equal(cache.get("a"), 10);
});

test("a miss does not change the order", () => {
  const cache = new LruCache<number, string>(3);
  cache.set(1, "x");
  cache.set(2, "y");
  assert.equal(cache.get(9), undefined);
  assert.deepEqual(cache.keys(), [1, 2]);
});

test("rejects a bad capacity", () => {
  assert.throws(() => new LruCache(0), RangeError);
});
