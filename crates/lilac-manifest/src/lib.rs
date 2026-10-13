//! `lilac.toml`, the project manifest (`docs/manifest-plan.md`): the TOML
//! reader it is written in ([`toml::read`]; nothing after it sees TOML
//! text) and the settings it holds ([`settings`]).

pub mod settings;
pub mod toml;

pub use lilac_module::MANIFEST_FILE;
pub use settings::{
    bundled_dir, bundled_libraries, default_name, is_program_name, Kind, Library, Manifest,
    Overrides, Setting, Settings, Spanned, TargetChoice, LIB_DIR_VAR, SECTIONS, SETTINGS,
};

#[cfg(test)]
mod settings_tests;
#[cfg(test)]
mod tests;
