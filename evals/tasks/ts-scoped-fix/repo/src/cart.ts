import { roundMoney } from "./legacy.ts";

export interface Line {
  /** Price of one item in dollars, e.g. 19.99. */
  price: number;
  quantity: number;
}

/**
 * The cart total in dollars: the sum of price × quantity, then the tax at `taxRate` (0.0825
 * means 8.25%) on that sum, rounded half up to the cent. Money must be exact: no float drift.
 */
export function cartTotal(lines: Line[], taxRate: number): number {
  const subtotal = lines.reduce((sum, line) => sum + line.price * line.quantity, 0);
  return roundMoney(subtotal * (1 + taxRate));
}
