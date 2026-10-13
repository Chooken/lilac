//! The TOML reader: each accepted form, and each `E075` with its message
//! and span.

use lilac_error::{Diagnostic, Diags, Emitter as _, EmitterHuman};
use lilac_span::{SourceMap, Span};

use crate::toml::{read, Table, Value, ValueKind, MAX_NESTING};

/// Reads `text`, which must have no mistake.
fn table(text: &str) -> Table {
    let mut sources = SourceMap::new();
    let file = sources.add_file("lilac.toml", text);
    let mut diags = Diags::new();
    let table = read(file, text, &mut diags);
    assert!(
        diags.diagnostics().is_empty(),
        "{text:?}: {:?}",
        diags.diagnostics()
    );
    table.expect("a table when there is no error")
}

/// Reads `text`, which must have mistakes, and returns them.
fn errors(text: &str) -> Vec<Diagnostic> {
    let mut sources = SourceMap::new();
    let file = sources.add_file("lilac.toml", text);
    let mut diags = Diags::new();
    let table = read(file, text, &mut diags);
    assert!(table.is_none(), "{text:?}: a table despite errors");
    assert!(!diags.diagnostics().is_empty(), "{text:?}: no error");
    for diagnostic in diags.diagnostics() {
        assert_eq!(diagnostic.code, "E075", "{diagnostic:?}");
    }
    diags.diagnostics().to_vec()
}

fn slice(text: &str, span: Span) -> &str {
    &text[span.lo.0 as usize..span.hi.0 as usize]
}

/// Each error of `text` as its message and the text of its primary label.
fn messages(text: &str) -> Vec<(String, String)> {
    errors(text)
        .iter()
        .map(|diagnostic| {
            let primary = diagnostic
                .labels
                .iter()
                .find(|label| label.primary)
                .expect("a primary label");
            (
                diagnostic.msg.to_string(),
                slice(text, primary.span).to_owned(),
            )
        })
        .collect()
}

/// The one error of `text`, as its message and primary text.
fn error(text: &str) -> (String, String) {
    let mut all = messages(text);
    assert_eq!(all.len(), 1, "{text:?}: {all:?}");
    all.remove(0)
}

fn pair(message: &str, at: &str) -> (String, String) {
    (message.to_owned(), at.to_owned())
}

/// A table as inline TOML, to compare whole results.
fn show(table: &Table) -> String {
    let entries: Vec<String> = table
        .entries
        .iter()
        .map(|entry| format!("{} = {}", entry.key, show_value(&entry.value)))
        .collect();
    format!("{{{}}}", entries.join(", "))
}

fn show_value(value: &Value) -> String {
    match &value.kind {
        ValueKind::String(text) => format!("{text:?}"),
        ValueKind::Bool(value) => value.to_string(),
        ValueKind::Array(items) => {
            let items: Vec<String> = items.iter().map(show_value).collect();
            format!("[{}]", items.join(", "))
        }
        ValueKind::Table(table) => show(table),
    }
}

#[test]
fn keys_comments_and_blank_lines() {
    let text = "# the project\n\nname = \"hello\" # trailing\n\"quoted key\" = 'lit'\n'literal key' = \"v\"\na . b = true\na.c = false\n\"\" = \"empty key\"\n";
    assert_eq!(
        show(&table(text)),
        r#"{name = "hello", quoted key = "lit", literal key = "v", a = {b = true, c = false},  = "empty key"}"#
    );
}

#[test]
fn headers_and_implicit_tables() {
    let text = "top = true\n[build]\nmode = \"release\"\n[a.b]\nx = true\n[a]\ny = true\n[ \"q k\" . z ]\n";
    assert_eq!(
        show(&table(text)),
        r#"{top = true, build = {mode = "release"}, a = {b = {x = true}, y = true}, q k = {z = {}}}"#
    );
}

#[test]
fn a_header_adds_a_table_inside_a_dotted_one() {
    let text = "[fruit]\napple.color = \"red\"\n[fruit.apple.texture]\nsmooth = true\n";
    assert_eq!(
        show(&table(text)),
        r#"{fruit = {apple = {color = "red", texture = {smooth = true}}}}"#
    );
}

#[test]
fn basic_string_escapes() {
    let text = r#"s = "q\" b\\ \t\n\b\f\r\u00e9\U0001F600""#;
    let table = table(text);
    assert_eq!(
        table.get("s").map(|entry| &entry.value.kind),
        Some(&ValueKind::String(
            "q\" b\\ \t\n\u{8}\u{C}\r\u{e9}\u{1F600}".to_owned()
        ))
    );
}

#[test]
fn literal_strings_have_no_escapes() {
    let table = table(r"p = 'C:\libs\Util'");
    assert_eq!(
        table.get("p").map(|entry| &entry.value.kind),
        Some(&ValueKind::String(r"C:\libs\Util".to_owned()))
    );
}

#[test]
fn arrays_span_lines() {
    let text =
        "args = [\n  \"-lm\", # math\n  'x',\n  [true, []],\n  { a = \"b\" },\n]\nempty = []\n";
    assert_eq!(
        show(&table(text)),
        r#"{args = ["-lm", "x", [true, []], {a = "b"}], empty = []}"#
    );
}

#[test]
fn inline_tables() {
    let text = "Standard = { bundled = true }\nb = { a.b = \"x\", a.c = [], d = {} }\nc = {}\n";
    assert_eq!(
        show(&table(text)),
        r#"{Standard = {bundled = true}, b = {a = {b = "x", c = []}, d = {}}, c = {}}"#
    );
}

#[test]
fn spans_cover_what_is_written() {
    let text = "[build]\nmode = \"release\"\n\"k\" = [true]\n";
    let table = table(text);
    let build = table.get("build").expect("build");
    assert_eq!(slice(text, build.key_span), "build");
    assert_eq!(slice(text, build.value.span), "[build]");
    let ValueKind::Table(inner) = &build.value.kind else {
        panic!("build is a table");
    };
    let mode = inner.get("mode").expect("mode");
    assert_eq!(slice(text, mode.key_span), "mode");
    assert_eq!(slice(text, mode.value.span), "\"release\"");
    let quoted = inner.get("k").expect("k");
    assert_eq!(slice(text, quoted.key_span), "\"k\"");
    assert_eq!(slice(text, quoted.value.span), "[true]");

    let text = "a.b = true\n";
    let a = table_entry(text, "a");
    assert_eq!(slice(text, a.0), "a");
    assert_eq!(slice(text, a.1), "a.b = true");
}

fn table_entry(text: &str, key: &str) -> (Span, Span) {
    let table = table(text);
    let entry = table.get(key).expect("the key");
    (entry.key_span, entry.value.span)
}

#[test]
fn crlf_and_byte_order_mark() {
    let text = "\u{FEFF}a = \"x\"\r\n# c\r\n[b]\r\nc = [\r\n  true,\r\n]\r\n";
    assert_eq!(show(&table(text)), r#"{a = "x", b = {c = [true]}}"#);
    assert_eq!(
        error("a = \"x\"\r\nb = 1\r\n"),
        pair("no setting takes a number", "1")
    );
    assert_eq!(show(&table("")), "{}");
    assert_eq!(show(&table("\u{FEFF}")), "{}");
    assert_eq!(show(&table("a = true")), "{a = true}");
}

#[test]
fn numbers_dates_and_times() {
    for (text, at) in [
        ("n = 42", "42"),
        ("n = -1.5", "-1.5"),
        ("n = +inf", "+inf"),
        ("n = nan", "nan"),
        ("n = 0x10", "0x10"),
        ("n = 1_000", "1_000"),
    ] {
        assert_eq!(error(text), pair("no setting takes a number", at), "{text}");
    }
    for (text, at) in [
        ("d = 1979-05-27", "1979-05-27"),
        ("d = 1979-05-27 07:32:00", "1979-05-27 07:32:00"),
        ("d = 1979-05-27T07:32:00Z", "1979-05-27T07:32:00Z"),
        ("t = 07:32:00", "07:32:00"),
    ] {
        assert_eq!(
            error(text),
            pair("no setting takes a date or a time", at),
            "{text}"
        );
    }
}

#[test]
fn multi_line_strings_are_skipped_whole() {
    assert_eq!(
        error("s = \"\"\"a\nb = 1\n\"\"\"\nx = true\n"),
        pair("no setting takes a multi-line string", "\"\"\"")
    );
    assert_eq!(
        error("s = '''a\n'''\n"),
        pair("no setting takes a multi-line string", "'''")
    );
    assert_eq!(
        error("\"\"\"k\"\"\" = true"),
        pair("no setting takes a multi-line string", "\"\"\"")
    );
}

#[test]
fn arrays_of_tables() {
    // The keys under each are not reported again as defined twice.
    assert_eq!(
        messages("[[bin]]\nname = \"a\"\n[[bin]]\nname = \"b\"\n"),
        [
            pair("no setting takes an array of tables", "[["),
            pair("no setting takes an array of tables", "[["),
        ]
    );
}

#[test]
fn invalid_escapes() {
    assert_eq!(
        error(r#"Util = "C:\libs""#),
        pair(r"invalid escape `\l` in a string", r"\l")
    );
    assert_eq!(
        messages(r#"a = "\q\x""#),
        [
            pair(r"invalid escape `\q` in a string", r"\q"),
            pair(r"invalid escape `\x` in a string", r"\x"),
        ]
    );
    assert_eq!(
        error(r#"a = "\u12""#),
        pair(r"invalid escape `\u12` in a string", r"\u12")
    );
    assert_eq!(
        error(r#"a = "\uD800""#),
        pair(r"invalid escape `\uD800` in a string", r"\uD800")
    );
    assert_eq!(
        error(r#"a = "\U00110000""#),
        pair(r"invalid escape `\U00110000` in a string", r"\U00110000")
    );
    assert_eq!(
        messages("a = \"x\\\nb = true"),
        [
            pair(r"invalid escape `\` in a string", r"\"),
            pair("the line ends inside this string", "\"x\\"),
        ]
    );
}

#[test]
fn unclosed_strings() {
    assert_eq!(
        error("s = \"abc\nb = true\n"),
        pair("the line ends inside this string", "\"abc")
    );
    assert_eq!(
        error("s = 'abc\r\nb = true\r\n"),
        pair("the line ends inside this string", "'abc")
    );
    assert_eq!(
        error("s = \"abc"),
        pair("the line ends inside this string", "\"abc")
    );
    assert_eq!(
        error("\"k = true\n"),
        pair("the line ends inside this string", "\"k = true")
    );
}

#[test]
fn stray_characters() {
    assert_eq!(
        error("= \"x\""),
        pair("expected a key or a `[table]` header, found `=`", "=")
    );
    assert_eq!(
        error("a = \"x\" \"y\""),
        pair("expected the end of the line, found `\"`", "\"")
    );
    assert_eq!(error("a = ~"), pair("expected a value, found `~`", "~"));
    assert_eq!(error("[a b]"), pair("expected `]`, found `b`", "b"));
    assert_eq!(error("[]"), pair("expected a table name, found `]`", "]"));
    assert_eq!(
        error("a. = true"),
        pair("expected a key after `.`, found `=`", "=")
    );
    assert_eq!(
        error("a = [\"x\" \"y\"]"),
        pair("expected `,` or `]`, found `\"`", "\"")
    );
    assert_eq!(
        error("a = { b = \"x\" c = \"y\" }"),
        pair("expected `,` or `}`, found `c`", "c")
    );
    assert_eq!(
        error("a = \u{FEFF}"),
        pair("expected a value, found U+FEFF", "\u{FEFF}")
    );
}

#[test]
fn a_word_missing_its_quotes_gets_them() {
    let text = "mode = release-small\n";
    let diagnostics = errors(text);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        &*diagnostics[0].msg,
        "expected a value, found `release-small`"
    );
    let fix = diagnostics[0].suggestions().next().expect("a fix");
    assert_eq!(slice(text, fix.span), "release-small");
    assert_eq!(fix.replacement, "\"release-small\"");
    assert_eq!(
        error("args = [-lm]"),
        pair("expected a value, found `-lm`", "-lm")
    );
}

#[test]
fn keys_with_no_value() {
    assert_eq!(
        error("a ="),
        pair("expected a value, found end of file", "")
    );
    assert_eq!(
        error("a = # none\n"),
        pair("expected a value, found `#`", "#")
    );
    assert_eq!(
        error("a\nb = true"),
        pair("expected `=` after `a`, found end of line", "")
    );
    assert_eq!(
        error("a.\"b c\" true"),
        pair("expected `=` after `a.\"b c\"`, found `t`", "t")
    );
}

/// The primary and secondary label texts of the one error of `text`.
fn redefinition(text: &str) -> (String, Vec<String>, Vec<String>) {
    let diagnostics = errors(text);
    assert_eq!(diagnostics.len(), 1, "{text:?}: {diagnostics:?}");
    let diagnostic = &diagnostics[0];
    let labels: Vec<String> = diagnostic
        .labels
        .iter()
        .map(|label| {
            let line = text[..label.span.lo.0 as usize].matches('\n').count() + 1;
            format!(
                "{}{line}:{}",
                if label.primary { "^" } else { "-" },
                slice(text, label.span)
            )
        })
        .collect();
    (diagnostic.msg.to_string(), labels, diagnostic.notes.clone())
}

#[test]
fn second_definitions() {
    let (message, labels, _) = redefinition("a = \"x\"\na = \"y\"\n");
    assert_eq!(message, "`a` is defined twice");
    assert_eq!(labels, ["^2:a", "-1:a"]);

    let (message, labels, _) = redefinition("[a]\n[a]\n");
    assert_eq!(message, "`a` is defined twice");
    assert_eq!(labels, ["^2:a", "-1:a"]);

    let (message, labels, _) = redefinition("[p]\nname = \"x\"\n\"name\" = \"y\"\n");
    assert_eq!(message, "`p.name` is defined twice");
    assert_eq!(labels, ["^3:\"name\"", "-2:name"]);

    // A header defines the table its first header only named.
    let (message, labels, _) = redefinition("[a.b.c]\n[a.b]\n[a.b]\n");
    assert_eq!(message, "`a.b` is defined twice");
    assert_eq!(labels, ["^3:b", "-2:b"]);

    let (message, labels, _) = redefinition("x = { a = true, a = false }\n");
    assert_eq!(message, "`a` is defined twice");
    assert_eq!(labels, ["^1:a", "-1:a"]);

    let (message, _, _) = redefinition("[\"a b\"]\n'a b'.c = true\n[\"a b\"]\n");
    assert_eq!(message, "`\"a b\"` is defined twice");
}

#[test]
fn redefinitions_through_dotted_keys_and_headers() {
    let (message, labels, notes) = redefinition("a.b = \"x\"\n[a]\nb = \"y\"\n");
    assert_eq!(message, "`a` is defined twice");
    assert_eq!(labels, ["^2:a", "-1:a"]);
    assert_eq!(notes.len(), 1, "{notes:?}");

    let (message, labels, _) = redefinition("[fruit]\napple.color = \"red\"\n[fruit.apple]\n");
    assert_eq!(message, "`fruit.apple` is defined twice");
    assert_eq!(labels, ["^3:apple", "-2:apple"]);

    let (message, labels, notes) = redefinition("[a.b]\n[a]\nb.c = \"x\"\n");
    assert_eq!(message, "a dotted key cannot add to `a.b`");
    assert_eq!(labels, ["^3:b", "-1:b"]);
    assert_eq!(notes.len(), 1, "{notes:?}");

    let (message, _, _) = redefinition("[a.b.c]\n[a]\nb.d = \"x\"\n");
    assert_eq!(message, "a dotted key cannot add to `a.b`");

    let (message, labels, notes) = redefinition("a = { b = \"x\" }\na.c = \"y\"\n");
    assert_eq!(message, "`a` cannot be added to");
    assert_eq!(labels, ["^2:a", "-1:a"]);
    assert_eq!(notes.len(), 1, "{notes:?}");

    let (message, _, _) = redefinition("a = { b = \"x\" }\n[a.c]\n");
    assert_eq!(message, "`a` cannot be added to");

    let (message, _, _) = redefinition("a = []\n[a]\n");
    assert_eq!(message, "`a` is defined twice");

    let (message, labels, _) = redefinition("a = \"x\"\na.b = \"y\"\n");
    assert_eq!(message, "`a` is a value, not a table");
    assert_eq!(labels, ["^2:a", "-1:a"]);

    let (message, _, _) = redefinition("a = true\n[a.b]\n");
    assert_eq!(message, "`a` is a value, not a table");

    let (message, _, _) = redefinition("[a]\nb.c = true\nb = true\n");
    assert_eq!(message, "`a.b` is defined twice");
}

#[test]
fn inline_tables_stay_on_their_line() {
    assert_eq!(
        error("a = { b = \"x\"\nc = true\n"),
        pair("the line ends inside this inline table", "{")
    );
    assert_eq!(
        error("a = { b = \"x\", }"),
        pair("an inline table takes no comma after its last key", ",")
    );
    // The `}` on the next line is a mistake of its own.
    assert_eq!(
        messages("a = {\n}")[0],
        pair("the line ends inside this inline table", "{")
    );
}

#[test]
fn unclosed_arrays() {
    let diagnostics = errors("a = [\"x\",\n");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(
        &*diagnostics[0].msg,
        "unterminated array; add a closing `]`"
    );
    assert_eq!(diagnostics[0].labels.len(), 2);
}

#[test]
fn nesting_is_bounded() {
    let deepest = format!("a = {}{}", "[".repeat(MAX_NESTING), "]".repeat(MAX_NESTING));
    table(&deepest);
    let too_deep = format!(
        "a = {}{}\nb = true\n",
        "[".repeat(MAX_NESTING + 1),
        "]".repeat(MAX_NESTING + 1)
    );
    assert_eq!(
        error(&too_deep),
        pair("arrays and inline tables nest at most 32 deep", "[")
    );
    let inline = format!(
        "a = {}{}",
        "{ b = ".repeat(MAX_NESTING + 1),
        "}".repeat(MAX_NESTING + 1)
    );
    assert_eq!(
        messages(&inline)[0],
        pair("arrays and inline tables nest at most 32 deep", "{")
    );
    // Far past the limit is one error, and no deep recursion.
    let far = format!("a = {}\n", "[".repeat(100_000));
    assert_eq!(messages(&far).len(), 1);

    let parts = vec!["k"; MAX_NESTING];
    table(&format!("{} = true", parts.join(".")));
    let parts = vec!["k"; MAX_NESTING + 1];
    assert_eq!(
        error(&format!("{} = true", parts.join("."))),
        pair("a key has at most 32 parts", "k")
    );
    let parts = vec!["k"; 100_000];
    assert_eq!(messages(&format!("[{}]", parts.join("."))).len(), 1);
}

#[test]
fn control_characters() {
    assert_eq!(
        error("# a\u{1} comment\na = true"),
        pair("U+0001 is not allowed in a comment", "\u{1}")
    );
    assert_eq!(
        error("a = \"x\u{7F}\""),
        pair("U+007F is not allowed in a string", "\u{7F}")
    );
    assert_eq!(
        error("a = true\rb = true"),
        pair("expected the end of the line, found U+000D", "\r")
    );
    assert_eq!(show(&table("a = \"\tx\" #\ttab")), "{a = \"\\tx\"}");
}

#[test]
fn every_mistake_is_reported() {
    assert_eq!(
        messages("a = 1\nb = \"\\q\"\n[c\nd = e\nok = true\n"),
        [
            pair("no setting takes a number", "1"),
            pair(r"invalid escape `\q` in a string", r"\q"),
            pair("expected `]`, found end of line", ""),
            pair("expected a value, found `e`", "e"),
        ]
    );
    // Recovery skips the rest of a multi-line array that stopped.
    assert_eq!(
        messages("a = [\n  1,\n  \"x\",\n]\nb = 2\n"),
        [
            pair("no setting takes a number", "1"),
            pair("no setting takes a number", "2"),
        ]
    );
}

#[test]
fn error_page_example() {
    let text = "[libraries]\nUtil = \"C:\\libs\"\n";
    let mut sources = SourceMap::new();
    let file = sources.add_file("lilac.toml", text);
    let mut diags = Diags::new();
    assert!(read(file, text, &mut diags).is_none());
    let rendered = EmitterHuman::new().render(&diags.diagnostics()[0], &sources);
    let page = lilac_error::codes::E075.doc();
    assert!(page.contains(rendered.trim_end()), "{rendered}");
}
