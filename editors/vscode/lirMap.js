"use strict";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function utf8ByteLength(text) {
  return encoder.encode(text).length;
}

function positionToByte(text, position) {
  const lines = text.split("\n");
  let byte = 0;
  for (let i = 0; i < position.line && i < lines.length; i++) {
    byte += utf8ByteLength(lines[i]) + 1;
  }
  const line = lines[position.line] ?? "";
  const prefix = line.slice(0, position.character);
  return byte + utf8ByteLength(prefix);
}

function byteToPosition(text, byte) {
  const lines = text.split("\n");
  let remaining = byte;
  for (let line = 0; line < lines.length; line++) {
    const lineBytes = utf8ByteLength(lines[line]);
    if (remaining <= lineBytes) {
      const prefix = encoder.encode(lines[line]).slice(0, remaining);
      return { line, character: decoder.decode(prefix).length };
    }
    remaining -= lineBytes + 1;
  }
  const last = lines.length - 1;
  return { line: last, character: (lines[last] ?? "").length };
}

function forward(spans, line) {
  for (const span of spans) {
    if (span[0] === line) {
      return { file: span[1], lo: span[2], hi: span[3] };
    }
  }
  return null;
}

function reverse(spans, file, byte) {
  const lines = [];
  for (const span of spans) {
    if (span[1] === file && span[2] <= byte && byte < span[3]) {
      lines.push(span[0]);
    }
  }
  lines.sort((a, b) => a - b);
  return lines.filter((line, index) => index === 0 || line !== lines[index - 1]);
}

module.exports = { positionToByte, byteToPosition, forward, reverse };
