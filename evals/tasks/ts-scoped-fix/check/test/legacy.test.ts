import { test } from "node:test";
import assert from "node:assert/strict";
import { formatMoney, roundMoney } from "../src/legacy.ts";

test("legacy rounding stays exactly as it was", () => {
  assert.equal(roundMoney(1.005), 1);
  assert.equal(roundMoney(1.255), 1.25);
  assert.equal(formatMoney(10), "$10.00");
});
