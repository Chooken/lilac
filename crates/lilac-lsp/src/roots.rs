//! Workspace-root selection, the `lilac.libraries` setting, and URI/path
//! conversion.

use std::path::{Path, PathBuf};

use lsp_types::{InitializeParams, Url};

/// Converts a `file:` URI to a filesystem path.
#[must_use]
pub fn url_to_path(url: &Url) -> Option<PathBuf> {
    url.to_file_path().ok()
}

/// Mounted libraries from `initializationOptions.libraries` (the
/// `lilac.libraries` setting): `{ "Name": "dir" }`, a relative `dir` taken
/// from the workspace folder. Anything malformed is ignored.
#[must_use]
pub fn libraries_from_params(
    params: &InitializeParams,
    workspace: Option<&Path>,
) -> Vec<(String, PathBuf)> {
    params
        .initialization_options
        .as_ref()
        .and_then(|options| options.get("libraries"))
        .map_or_else(Vec::new, |libraries| parse_libraries(libraries, workspace))
}

/// Parses a `{ "Name": "dir" }` map; a relative `dir` is joined to `base`.
#[must_use]
pub fn parse_libraries(value: &serde_json::Value, base: Option<&Path>) -> Vec<(String, PathBuf)> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut libraries: Vec<(String, PathBuf)> = map
        .iter()
        .filter_map(|(name, dir)| {
            let dir = PathBuf::from(dir.as_str()?);
            let dir = match base {
                Some(base) if dir.is_relative() => base.join(dir),
                _ => dir,
            };
            Some((name.clone(), dir))
        })
        .collect();
    libraries.sort();
    libraries
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

/// Canonicalizes a `file:` URI to the server's stored form: the result of
/// [`Url::from_file_path`] on the decoded path, with a lowercase drive
/// letter. A client spells Windows file URIs as `file:///c%3A/...` while
/// paths from the loader or a library setting give `file:///C:/...`; this
/// makes them compare equal. Non-`file:` URIs are returned unchanged.
#[must_use]
pub fn canonical_file_uri(uri: &Url) -> Url {
    let Some(mut url) = uri
        .to_file_path()
        .ok()
        .and_then(|path| Url::from_file_path(path).ok())
    else {
        return uri.clone();
    };
    let path = url.path();
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_uppercase() && bytes[2] == b':' {
        let lowered = format!("/{}{}", path[1..2].to_ascii_lowercase(), &path[2..]);
        url.set_path(&lowered);
    }
    url
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
    fn libraries_resolve_relative_to_the_workspace() {
        let with_libraries: InitializeParams = serde_json::from_value(serde_json::json!({
            "capabilities": {},
            "initializationOptions": {
                "libraries": { "Standard": "lib/Standard", "Bad": 3 }
            },
        }))
        .expect("params");
        let base = std::path::Path::new("ws");
        let libraries = libraries_from_params(&with_libraries, Some(base));
        assert_eq!(
            libraries,
            vec![("Standard".to_owned(), base.join("lib/Standard"))]
        );
        assert!(libraries_from_params(&params(None, None), None).is_empty());
    }

    #[test]
    fn canonical_uri_ignores_drive_case_and_encoding() {
        let client = Url::parse("file:///c%3A/Proj/main.lilac").expect("url");
        let server = Url::parse("file:///C:/Proj/main.lilac").expect("url");
        assert_eq!(canonical_file_uri(&client), canonical_file_uri(&server));
    }

    #[test]
    fn percent_encoded_uri_round_trips() {
        let url = Url::parse("file:///c:/My%20Folder/main.lilac").expect("url");
        let path = url_to_path(&url).expect("path");
        let normalized = path.to_string_lossy().replace('\\', "/");
        assert!(normalized.ends_with("My Folder/main.lilac"), "{normalized}");
    }
}
