const test = require("node:test");
const assert = require("node:assert");
const { positionToByte, byteToPosition, forward, reverse } = require("../lirMap.js");

test("positionToByte counts UTF-8 bytes, not UTF-16 units", () => {
  const text = "ab\né!\n";
  assert.strictEqual(positionToByte(text, { line: 1, character: 0 }), 3);
  assert.strictEqual(positionToByte(text, { line: 1, character: 1 }), 5); // é is 2 bytes
});

test("byteToPosition inverts positionToByte", () => {
  const text = "ab\né!\n";
  for (const p of [{ line: 0, character: 0 }, { line: 1, character: 1 }, { line: 2, character: 0 }]) {
    const byte = positionToByte(text, p);
    assert.deepStrictEqual(byteToPosition(text, byte), p);
  }
});

test("forward finds the span for a line", () => {
  const spans = [[12, 0, 40, 44], [13, 1, 5, 9]];
  assert.deepStrictEqual(forward(spans, 12), { file: 0, lo: 40, hi: 44 });
  assert.strictEqual(forward(spans, 99), null);
});

test("reverse returns all lines containing a byte", () => {
  const spans = [[12, 0, 40, 44], [13, 0, 40, 50], [14, 1, 0, 3]];
  assert.deepStrictEqual(reverse(spans, 0, 41), [12, 13]);
  assert.deepStrictEqual(reverse(spans, 0, 3), []);
});
