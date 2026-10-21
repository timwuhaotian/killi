import { test } from "node:test";
import assert from "node:assert/strict";
import { cartTotal } from "../src/cart.ts";

test("simple carts", () => {
  assert.equal(cartTotal([{ price: 10, quantity: 2 }], 0), 20);
  assert.equal(cartTotal([{ price: 19.99, quantity: 3 }], 0.0825), 64.92);
});

test("rounds half up to the cent without float drift", () => {
  // 0.1 * 3 = 0.30000000000000004 in floats; tax 0 -> 0.30.
  assert.equal(cartTotal([{ price: 0.1, quantity: 3 }], 0), 0.3);
  // 1.005 exactly -> 1.01 (half up), unlike the legacy helper.
  assert.equal(cartTotal([{ price: 1.005, quantity: 1 }], 0), 1.01);
  // 0.285 -> 0.29 and 1.255 -> 1.26, where the legacy helper rounds down.
  assert.equal(cartTotal([{ price: 0.285, quantity: 1 }], 0), 0.29);
  assert.equal(cartTotal([{ price: 1.255, quantity: 1 }], 0), 1.26);
  // 4.35 * 1.10 = 4.785 -> 4.79.
  assert.equal(cartTotal([{ price: 4.35, quantity: 1 }], 0.1), 4.79);
});
