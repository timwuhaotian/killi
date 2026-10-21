/**
 * Rounds a money amount to cents. Kept as it has always been: other services reproduce
 * these numbers exactly, including the float behavior (1.005 rounds to 1.00).
 */
export function roundMoney(amount: number): number {
  return Math.round(amount * 100) / 100;
}

/** Formats an amount as dollars with two decimals. */
export function formatMoney(amount: number): string {
  return `$${roundMoney(amount).toFixed(2)}`;
}
