//! Workspace-root selection and URI/path conversion.

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

/// Merges two library lists; `overrides` wins on a name both mount.
#[must_use]
pub fn merge_libraries(
    base: Vec<(String, PathBuf)>,
    overrides: Vec<(String, PathBuf)>,
) -> Vec<(String, PathBuf)> {
    let mut libraries: Vec<(String, PathBuf)> = base
        .into_iter()
        .filter(|(name, _)| !overrides.iter().any(|(other, _)| other == name))
        .collect();
    libraries.extend(overrides);
    libraries.sort();
    libraries
}

/// The workspace's `lilac.json`, if it exists and parses:
///
/// ```json
/// { "root": "src", "libraries": { "Standard": "lib/Standard" } }
/// ```
fn config(workspace: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(workspace.join("lilac.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// The libraries `lilac.json` mounts (`"libraries": { "Name": "dir" }`, a
/// relative `dir` taken from the workspace folder).
#[must_use]
pub fn config_libraries(workspace: &Path) -> Vec<(String, PathBuf)> {
    config(workspace)
        .and_then(|value| value.get("libraries").cloned())
        .map_or_else(Vec::new, |libraries| {
            parse_libraries(&libraries, Some(workspace))
        })
}

/// The effective project root for a workspace folder: the directory named by
/// `lilac.json`'s `root` field when that file exists, parses, and points at a
/// directory; otherwise the workspace folder itself. `root` may be relative to
/// the workspace folder or absolute.
#[must_use]
pub fn project_root(workspace: &Path) -> PathBuf {
    let Some(value) = config(workspace) else {
        return workspace.to_path_buf();
    };
    let Some(root) = value.get("root").and_then(serde_json::Value::as_str) else {
        return workspace.to_path_buf();
    };
    let root = root.trim();
    if root.is_empty() {
        return workspace.to_path_buf();
    }
    let path = Path::new(root);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    if candidate.is_dir() {
        candidate
    } else {
        workspace.to_path_buf()
    }
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
    fn config_libraries_merge_under_the_setting() {
        let dir = temp_dir("cfg-libs");
        std::fs::write(
            dir.join("lilac.json"),
            r#"{ "libraries": { "Standard": "lib/Standard", "Extra": "x" } }"#,
        )
        .expect("write");
        let from_config = config_libraries(&dir);
        assert_eq!(
            from_config,
            vec![
                ("Extra".to_owned(), dir.join("x")),
                ("Standard".to_owned(), dir.join("lib/Standard")),
            ]
        );
        let setting = vec![("Standard".to_owned(), PathBuf::from("other"))];
        assert_eq!(
            merge_libraries(from_config, setting),
            vec![
                ("Extra".to_owned(), dir.join("x")),
                ("Standard".to_owned(), PathBuf::from("other")),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lilac-lsp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn project_root_reads_a_relative_config_root() {
        let dir = temp_dir("cfg-rel");
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).expect("mkdir");
        std::fs::write(dir.join("lilac.json"), r#"{ "root": "proj" }"#).expect("write");
        assert_eq!(project_root(&dir), proj);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_root_reads_an_absolute_config_root() {
        let dir = temp_dir("cfg-abs");
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).expect("mkdir");
        let json = format!(
            r#"{{ "root": "{}" }}"#,
            proj.to_string_lossy().replace('\\', "\\\\")
        );
        std::fs::write(dir.join("lilac.json"), json).expect("write");
        assert_eq!(project_root(&dir), proj);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_root_falls_back_without_a_usable_config() {
        let dir = temp_dir("cfg-fallback");
        assert_eq!(project_root(&dir), dir, "no config");
        std::fs::write(dir.join("lilac.json"), "{ not json").expect("write");
        assert_eq!(project_root(&dir), dir, "malformed json");
        std::fs::write(dir.join("lilac.json"), r#"{ "other": 1 }"#).expect("write");
        assert_eq!(project_root(&dir), dir, "no root field");
        std::fs::write(dir.join("lilac.json"), r#"{ "root": "  " }"#).expect("write");
        assert_eq!(project_root(&dir), dir, "empty root");
        std::fs::write(dir.join("lilac.json"), r#"{ "root": "missing" }"#).expect("write");
        assert_eq!(project_root(&dir), dir, "nonexistent root");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
