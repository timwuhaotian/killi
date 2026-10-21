/**
 * Parses CSV text into rows of fields (RFC 4180).
 *
 * - Fields are separated by commas and rows by `\n` or `\r\n`.
 * - A field wrapped in double quotes may contain commas, line breaks and `""`, which stands
 *   for one double quote. Quotes inside an unquoted field are kept as they are.
 * - Empty fields are kept: `a,,b` is three fields, and `,` is two empty fields.
 * - A line break at the very end of the text does not start another row. Empty text has no rows.
 * - An opening quote that is never closed is an error: throw a SyntaxError.
 */
export function parseCsv(text: string): string[][] {
  throw new Error("not implemented");
}
