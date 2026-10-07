//! Workspace-root selection and URI/path conversion.

use std::path::PathBuf;

use lsp_types::{InitializeParams, Url};

/// Converts a `file:` URI to a filesystem path.
#[must_use]
pub fn url_to_path(url: &Url) -> Option<PathBuf> {
    url.to_file_path().ok()
}

/// The project root: the first workspace folder, else `rootUri`.
#[must_use]
#[allow(deprecated)] // `root_uri` is deprecated in the protocol but still sent by older clients.
pub fn root_from_params(params: &InitializeParams) -> Option<PathBuf> {
    if let Some(folder) = params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
    {
        return url_to_path(&folder.uri);
    }
    params.root_uri.as_ref().and_then(url_to_path)
}

/// Canonicalizes a `file:` URI to the server's stored form — the result of
/// [`Url::from_file_path`] on the decoded path. A client spells Windows file
/// URIs as `file:///c%3A/...` (lowercase drive, percent-encoded colon) while
/// the server stores `file:///C:/...`; this makes the two compare equal.
/// Non-`file:` URIs are returned unchanged.
#[must_use]
pub fn canonical_file_uri(uri: &Url) -> Url {
    uri.to_file_path()
        .ok()
        .and_then(|path| Url::from_file_path(path).ok())
        .unwrap_or_else(|| uri.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{InitializeParams, Url};

    fn params(root_uri: Option<&str>, folder: Option<&str>) -> InitializeParams {
        serde_json::from_value(serde_json::json!({
            "capabilities": {},
            "rootUri": root_uri,
            "workspaceFolders": folder.map(|uri| serde_json::json!([{ "uri": uri, "name": "ws" }])),
        }))
        .expect("params")
    }

    #[test]
    fn workspace_folder_beats_root_uri() {
        let params = params(Some("file:///other"), Some("file:///c:/My%20Folder"));
        let root = root_from_params(&params).expect("root");
        let expected =
            url_to_path(&Url::parse("file:///c:/My%20Folder").expect("url")).expect("path");
        assert_eq!(root, expected);
    }

    #[test]
    fn root_uri_used_when_no_folders() {
        let params = params(Some("file:///c:/proj"), None);
        let expected = url_to_path(&Url::parse("file:///c:/proj").expect("url"));
        assert!(expected.is_some());
        assert_eq!(root_from_params(&params), expected);
    }

    #[test]
    fn percent_encoded_uri_round_trips() {
        let url = Url::parse("file:///c:/My%20Folder/main.lilac").expect("url");
        let path = url_to_path(&url).expect("path");
        let normalized = path.to_string_lossy().replace('\\', "/");
        assert!(normalized.ends_with("My Folder/main.lilac"), "{normalized}");
    }
}
