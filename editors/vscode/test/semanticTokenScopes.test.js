const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

// Must match `crates/lilac-lsp/src/lib.rs` TOKEN_TYPES, in legend index order.
const LEGEND_TOKEN_TYPES = [
  "function",
  "type",
  "enum",
  "property",
  "enumMember",
  "variable",
  "parameter",
];

const EXPECTED_SCOPES = {
  function: ["entity.name.function"],
  type: ["entity.name.type"],
  enum: ["entity.name.type.enum"],
  property: ["variable.other.property"],
  enumMember: ["variable.other.enummember"],
  variable: ["variable.other"],
  parameter: ["variable.parameter"],
};

const pkg = JSON.parse(
  fs.readFileSync(path.join(__dirname, "..", "package.json"), "utf8"),
);

test("semanticTokenScopes maps every legend token type to a non-empty scope list", () => {
  const scopes = pkg.contributes && pkg.contributes.semanticTokenScopes;
  assert.ok(scopes, "contributes.semanticTokenScopes is present");
  for (const tokenType of LEGEND_TOKEN_TYPES) {
    const entry = scopes.find((s) => (s.language === undefined || s.language === "lilac") && s.scopes[tokenType]);
    assert.ok(entry, `a scopes entry covers token type '${tokenType}'`);
    assert.ok(
      Array.isArray(entry.scopes[tokenType]) && entry.scopes[tokenType].length > 0,
      `'${tokenType}' maps to a non-empty scope list`,
    );
  }
});

test("semanticTokenScopes uses the standard scopes from the design", () => {
  const scopes = pkg.contributes.semanticTokenScopes;
  const entry = scopes.find((s) => s.language === undefined || s.language === "lilac");
  assert.deepStrictEqual(entry.scopes, EXPECTED_SCOPES);
});
