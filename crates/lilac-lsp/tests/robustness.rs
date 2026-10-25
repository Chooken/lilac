//! What the editor says comes from tokens, the syntax tree and the
//! compiler's tables, never from scanning text (`docs/editor-plan.md`).

use std::collections::HashMap;

use lilac_lsp::convert::LineIndex;
use lilac_lsp::documents::{map_offset, Documents, Edit};
use lilac_lsp::index::Retained;
use lilac_lsp::{complete, handlers};
use lilac_span::FileId;
use lsp_types::{
    DidChangeTextDocumentParams, HoverContents, ParameterLabel, Position, Range, SignatureHelp,
    TextDocumentContentChangeEvent, TextDocumentItem, Url, VersionedTextDocumentIdentifier,
};

const PROGRAM: &str = "\
P: structure {
    x: @bit8
}
f: function (p: P, n: @byte_buffer) nothing {
}
limit: @bit8 = @i_add_8(0x1, // one
    0x2)
main: function () nothing {
    point: P = P { x = limit }
    // EDIT
}
";

fn uri() -> Url {
    Url::parse("file:///c:/main.lilac").expect("uri")
}

/// `PROGRAM` compiled and retained as a single file.
fn retained() -> Retained {
    let mut session = lilac_driver::Session::new();
    session.keep_analysis = lilac_driver::Keep::Analysis;
    let _ = session.compile_lir("main.lilac", PROGRAM);
    Retained::build(
        session,
        HashMap::from([(FileId(0), uri())]),
        HashMap::from([(uri(), FileId(0))]),
    )
}

/// `PROGRAM` with the `// EDIT` line replaced, and the position at the end
/// of `edit`.
fn edited(edit: &str) -> (String, Position) {
    let text = PROGRAM.replace("    // EDIT", edit);
    let byte = text.find(edit).expect("edit") + edit.len();
    let before = &text[..byte];
    let line = u32::try_from(before.matches('\n').count()).expect("line");
    let character = before.len() - before.rfind('\n').map_or(0, |newline| newline + 1);
    let character = u32::try_from(character).expect("character");
    (text, Position::new(line, character))
}

fn signature(retained: &Retained, edit: &str) -> SignatureHelp {
    let (text, at) = edited(edit);
    let edits = [Edit::between(PROGRAM, &text, 2)];
    complete::signature_help(retained, &uri(), &LineIndex::new(&text), &edits, at)
        .expect("a signature")
}

/// The text of the parameter signature help highlights.
fn active(help: &SignatureHelp) -> String {
    let signature = &help.signatures[0];
    let index = help.active_parameter.expect("active") as usize;
    let parameter = &signature.parameters.as_ref().expect("parameters")[index];
    match &parameter.label {
        ParameterLabel::Simple(text) => text.clone(),
        ParameterLabel::LabelOffsets([start, end]) => {
            let units: Vec<u16> = signature.label.encode_utf16().collect();
            String::from_utf16(&units[*start as usize..*end as usize]).expect("utf-16")
        }
    }
}

#[test]
fn intrinsic_signature_help_highlights_the_parameter() {
    let retained = retained();
    let help = signature(&retained, "    @format_args_hole(args, 0x0, ");
    assert_eq!(help.active_parameter, Some(2));
    assert_eq!(active(&help), "out: ref W");
}

#[test]
fn signature_help_reads_past_struct_literals_and_strings() {
    let retained = retained();
    let help = signature(&retained, "    f(P { x = 0x1 }, ");
    assert_eq!(help.active_parameter, Some(1));
    assert_eq!(active(&help), "n: @byte_buffer");

    let help = signature(&retained, "    f(point, \"a)\" ");
    assert_eq!(help.active_parameter, Some(1));
    assert_eq!(active(&help), "n: @byte_buffer");
}

#[test]
fn a_dot_in_a_string_is_not_a_member() {
    let (text, at) = edited("    f(point, \"a//b\") point.");
    assert!(complete::needs_current_compile(&LineIndex::new(&text), at));
    let (text, at) = edited("    s: @byte_buffer = \"x.");
    assert!(!complete::needs_current_compile(&LineIndex::new(&text), at));
}

#[test]
fn a_hover_never_shows_source_comments() {
    let retained = retained();
    let (text, _) = edited("");
    let byte = text.find("x = limit").expect("use") + "x = ".len();
    let line = u32::try_from(text[..byte].matches('\n').count()).expect("line");
    let at = Position::new(line, 23);
    let hover = handlers::hover(&retained, &uri(), at).expect("hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("markup: {hover:?}");
    };
    assert!(markup.value.contains("limit"), "{}", markup.value);
    assert!(!markup.value.contains("//"), "{}", markup.value);
}

#[test]
fn separate_edits_map_offsets_between_them() {
    // Two edits; the cursor is in the unchanged text between them.
    let mut documents = Documents::new();
    documents.open(&TextDocumentItem {
        uri: uri(),
        language_id: "lilac".to_owned(),
        version: 1,
        text: "one two three".to_owned(),
    });
    for (version, (start, end, text)) in [(0, 3, "ONE"), (8, 13, "THREE")].into_iter().enumerate() {
        documents.change(&DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri(),
                version: i32::try_from(version).expect("version") + 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, start), Position::new(0, end))),
                range_length: None,
                text: text.to_owned(),
            }],
        });
    }
    let document = documents.get(&uri()).expect("open");
    assert_eq!(document.text, "ONE two THREE");
    assert_eq!(map_offset(document.edits_after(Some(1)), 4), 4);
    // Inside the second edit: its start.
    assert_eq!(map_offset(document.edits_after(Some(2)), 10), 8);
}
