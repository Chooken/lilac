//! `lilac.toml`, the project manifest (`docs/manifest-plan.md`): the TOML
//! reader it is written in ([`toml::read`]; nothing after it sees TOML
//! text), the settings it holds ([`settings`]), and the project
//! templates `lilac init` copies ([`template`]).

pub mod settings;
pub mod template;
pub mod toml;

pub use lilac_module::MANIFEST_FILE;
pub use settings::{
    bundled_dir, bundled_libraries, default_name, is_program_name, Kind, Library, Manifest,
    Overrides, Setting, Settings, Spanned, TargetChoice, LIB_DIR_VAR, SECTIONS, SETTINGS,
};
pub use template::{fill, read_template, template_dir, Fill, PROJECT_TEMPLATE, TEMPLATE_DIR_VAR};

#[cfg(test)]
mod settings_tests;
#[cfg(test)]
mod tests;
