export interface Line {
  price: number;
  quantity: number;
}

/** Rounds half up at the cent, working in decimal strings so float drift can't move it. */
function roundHalfUp(amount: number): number {
  const fixed = amount.toFixed(10);
  const [whole, fraction = ""] = fixed.split(".");
  const cents = BigInt(whole + fraction.slice(0, 2).padEnd(2, "0"));
  const rest = fraction.slice(2);
  const up = rest.length > 0 && Number(rest[0]) >= 5 ? 1n : 0n;
  return Number(cents + up) / 100;
}

export function cartTotal(lines: Line[], taxRate: number): number {
  const subtotal = lines.reduce((sum, line) => sum + line.price * line.quantity, 0);
  return roundHalfUp(subtotal * (1 + taxRate));
}
