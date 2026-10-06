//! Builds `lilac/lir` JSON payloads from retained captured IR stages.

use std::collections::BTreeMap;

use crate::index::Retained;
use lsp_types::Url;
use serde_json::{json, Value};

/// Renders the captured IR `stage` as a `lilac/lir` result payload.
///
/// Returns `None` when the compile never reached `stage`. The result carries the
/// canonical module `text`, `spans` (`[line, fileId, lo, hi]` for every
/// source-backed line), and `files` (`"<fileId>": "<uri>"` for every known file).
#[must_use]
pub fn lir_payload(retained: &Retained, _uri: &Url, stage: &str) -> Option<Value> {
    let module = retained
        .stages
        .modules
        .iter()
        .find(|(name, _)| *name == stage)
        .map(|(_, module)| module)?;
    let spanned = module.to_text_spanned();
    let spans: Vec<[u32; 4]> = spanned
        .lines
        .iter()
        .enumerate()
        .filter_map(|(line, span)| {
            let span = span.as_ref()?;
            if span.is_dummy() {
                return None;
            }
            let line = u32::try_from(line).ok()?;
            Some([line, span.file.0, span.lo.0, span.hi.0])
        })
        .collect();
    let files: BTreeMap<String, &str> = retained
        .file_uris
        .iter()
        .map(|(id, uri)| (id.0.to_string(), uri.as_str()))
        .collect();
    Some(json!({
        "stage": stage,
        "text": spanned.text,
        "spans": spans,
        "files": files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{LirStages, Retained};
    use lilac_span::FileId;
    use lsp_types::Url;
    use std::collections::HashMap;

    #[test]
    fn payload_reports_stage_text_spans_and_files() {
        let mut session = lilac_driver::Session::new();
        let mut stages = LirStages::default();
        let _ = session.compile_lir_with_stages(
            "t.lilac",
            "main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n",
            &mut |name, lir| stages.modules.push((name, lir.clone())),
        );
        let uri = Url::parse("file:///c:/t.lilac").expect("uri");
        let other = Url::parse("file:///c:/other.lilac").expect("uri");
        // Two files: `files` must carry every FileId, not just the opened one.
        let retained = Retained::build_with_stages(
            session,
            HashMap::from([(FileId(0), uri.clone()), (FileId(1), other.clone())]),
            HashMap::from([(uri.clone(), FileId(0)), (other.clone(), FileId(1))]),
            stages,
        );

        let value = lir_payload(&retained, &uri, "final").expect("payload");
        assert_eq!(value["stage"], "final");
        assert!(value["text"]
            .as_str()
            .expect("text")
            .contains("function @main"));
        assert_eq!(value["files"]["0"], uri.as_str());
        assert_eq!(value["files"]["1"], other.as_str());
        let spans = value["spans"].as_array().expect("spans");
        assert!(!spans.is_empty());
        assert_eq!(spans[0].as_array().expect("row").len(), 4);
    }

    #[test]
    fn payload_is_none_for_an_unknown_stage() {
        let retained =
            Retained::build(lilac_driver::Session::new(), HashMap::new(), HashMap::new());
        let uri = Url::parse("file:///c:/t.lilac").expect("uri");
        assert!(lir_payload(&retained, &uri, "nope").is_none());
    }
}
