import { test } from "node:test";
import assert from "node:assert/strict";
import { parseCsv } from "../src/csv.ts";

test("plain rows", () => {
  assert.deepEqual(parseCsv("a,b,c\n1,2,3"), [["a", "b", "c"], ["1", "2", "3"]]);
  assert.deepEqual(parseCsv("a,b\r\nc,d\r\n"), [["a", "b"], ["c", "d"]]);
  assert.deepEqual(parseCsv(""), []);
});

test("empty fields are kept", () => {
  assert.deepEqual(parseCsv("a,,b"), [["a", "", "b"]]);
  assert.deepEqual(parseCsv(","), [["", ""]]);
  assert.deepEqual(parseCsv("x\n\ny"), [["x"], [""], ["y"]]);
});

test("quoted fields", () => {
  assert.deepEqual(parseCsv('"a,b",c'), [["a,b", "c"]]);
  assert.deepEqual(parseCsv('"say ""hi""",2'), [['say "hi"', "2"]]);
  assert.deepEqual(parseCsv('"line 1\nline 2",x\r\ny,z'), [["line 1\nline 2", "x"], ["y", "z"]]);
  assert.deepEqual(parseCsv('"",""'), [["", ""]]);
  assert.deepEqual(parseCsv('ab"c,d'), [['ab"c', "d"]]);
});

test("an unclosed quote is an error", () => {
  assert.throws(() => parseCsv('"abc,d'), SyntaxError);
});
