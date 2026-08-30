//! frost's binding to the shared workflow library.
//!
//! The subsystem — the five-tier search path, the bounded reader, serde
//! deserialisation of both TOML and YAML, validation, the `{name}` /
//! `{{name}}` template engine and the parameter-fill model — lives in
//! [`jterm_core::workflows`]. That module is the union of the four jterm
//! terminals' previously duplicated copies (anvil, forge, ember and this one),
//! ~4,259 lines whose production half contained no toolkit code at all.
//!
//! The on-disk format is the whole point of this subsystem: the four apps read
//! the same library out of the same directories, so a difference in what one
//! app *accepts* was a difference in what a user's file *means* depending on
//! which terminal opened it. frost's copy contributed two of the union's
//! guards — `O_NOFOLLOW` in the bounded reader, so a symlink planted in a
//! scanned directory is refused at `open` rather than followed to a
//! world-writable target, and the `Option`-returning user-config lookup, so
//! `HOME` being unset skips the tier instead of scanning `./.config/…`
//! relative to wherever the process was started (forge did the latter). Both
//! are now the family's behaviour.
//!
//! What is left here is the policy frost owns, and only that:
//!
//! - **The search path** ([`workflow_dirs`]). [`SearchPathSpec::for_app`]
//!   derives the directory segment *and* the override variable from one name,
//!   so frost cannot look under `frost/` while honouring another app's
//!   variable. The dev-tree tier is passed in rather than computed in the core
//!   because `env!("CARGO_MANIFEST_DIR")` is resolved at compile time against
//!   the crate being compiled: evaluating it in `jterm_core` would point all
//!   four apps at `jterm_core/scripts/workflows`, which does not exist, while
//!   their bundled-library tests kept passing.
//! - **The XDG backend** ([`XdgEnvDirs`]). frost has no GTK dependency and
//!   asks the `dirs` crate plus `XDG_DATA_DIRS`; anvil and forge ask glib,
//!   whose lookups never fail. Injecting it is what keeps those two apps'
//!   directories from silently changing.
//! - **The load order** ([`LOAD_ORDER`]). frost lists in directory-precedence
//!   order so the user's own files head the picker; ember and forge sort the
//!   whole library by name. [`LoadOrder`] deliberately has no `Default` — the
//!   difference used to be the presence or absence of one `sort_by` line, and
//!   a silent core default is how two apps inherit a behaviour nobody chose.
//!
//! # What changed for a frost user
//!
//! An argument the file declares with **no default** is no longer filled by a
//! blank string. All four UIs pre-seeded every declared argument with `""`,
//! which made [`render`]'s `missing values:` guard — implemented and
//! unit-tested right here — unreachable from every terminal in the family:
//! `kill -9 {pid}` with an untouched Pid field rendered `kill -9 ` and was
//! typed at the prompt. An empty value is meaningful only if the file says so,
//! and `default = ""` is how a file says so. `workflow_picker`'s form shows
//! the outstanding rows before Insert; the old test asserting the weaker
//! behaviour was the fossil record of the defect and is gone.
//!
//! An unterminated `{{` also survives a template that closes a pair later on:
//! the close is matched by counting `{{`/`}}` nesting instead of scanning to
//! the end, so `awk '{{print $1}' {{log}}` no longer lets the first brace pair
//! claim the second placeholder's close and hand the user a different,
//! executable awk program. A declared argument name is now held to the same
//! spelling as the placeholder it must bind (both trimmed), so a quoted
//! `name = "pid "` is rejected rather than loading clean and silently
//! discarding whatever the user typed into that row. Every path a workflow
//! file's own bytes reach a log line through is sanitised, not just the
//! filename half.

use std::path::PathBuf;

use jterm_core::workflows::{search_path, LoadOrder, SearchPathSpec, XdgEnvDirs};

pub(crate) use jterm_core::workflows::{render, ArgsForm, Workflow};

/// Only the picker's own form test builds a workflow argument by hand — every
/// production path gets them from a file — so the re-export is compiled for
/// tests, the way this module's `substitute` used to be.
#[cfg(test)]
pub(crate) use jterm_core::workflows::WorkflowArg;

/// The path segment under every XDG base directory, and — derived from it by
/// [`SearchPathSpec::for_app`] — the `FROST_WORKFLOW_DIR` override variable.
const APP: &str = "frost";

/// See the module docs: frost lists in directory-precedence order, so
/// `~/.config/frost/workflows/` heads the picker and can shadow an installed
/// example by name. Stated here because the core has no default to inherit.
const LOAD_ORDER: LoadOrder = LoadOrder::Precedence;

/// The source-tree examples, the lowest-precedence tier. `env!` must be
/// expanded in this crate — see the module docs.
fn bundled_library() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("workflows")
}

/// frost's half of the search path: the app segment, the override variable it
/// implies, and where this checkout keeps its examples.
fn search_path_spec() -> SearchPathSpec {
    SearchPathSpec::for_app(APP, Some(bundled_library()))
}

/// Workflow search path in precedence order: `~/.config/frost/workflows/`,
/// `$FROST_WORKFLOW_DIR`, the XDG data directories, then the bundled examples.
pub(crate) fn workflow_dirs() -> Vec<PathBuf> {
    search_path(&search_path_spec(), &XdgEnvDirs)
}

/// Load a library over an explicit search path — normally [`workflow_dirs`],
/// and an explicit directory from the picker's test seam.
///
/// This is the one site that names [`LOAD_ORDER`], which is why every caller
/// goes through it rather than through `jterm_core::workflows::load_all`: the
/// core has no default order to fall back on, and frost should have exactly
/// one place where its answer can be read or changed.
pub(crate) fn load_library_from(dirs: &[PathBuf]) -> Vec<Workflow> {
    jterm_core::workflows::load_all(dirs, LOAD_ORDER)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every policy value frost states. The engine's own behaviour is tested
    /// in `jterm_core::workflows`; what can only be wrong *here* is which
    /// directories this app reads and in what order, and none of it has a
    /// default that would let the shim stay silent and still compile.
    #[test]
    fn frost_pins_its_search_path_and_its_load_order() {
        let spec = search_path_spec();
        assert_eq!(spec.app(), APP);
        // Derived, not spelled twice: an app cannot look under one name while
        // honouring another's override.
        assert_eq!(spec.env_var(), "FROST_WORKFLOW_DIR");
        assert_eq!(spec.dev_root(), Some(bundled_library().as_path()));
        assert_eq!(LOAD_ORDER, LoadOrder::Precedence);

        let dirs = workflow_dirs();
        assert!(dirs.iter().all(|dir| dir.is_absolute()));
        assert!(dirs.contains(&bundled_library()));
        if let Some(data_dir) = dirs::data_dir() {
            assert!(dirs.contains(&data_dir.join(APP).join("workflows")));
        }
        // The dev tree is the lowest-precedence tier, and every other tier —
        // user config, user data, and *each* system data directory — carries
        // frost's own segment. Conditional because a developer's own
        // $FROST_WORKFLOW_DIR is kept verbatim and may sit anywhere.
        if std::env::var_os("FROST_WORKFLOW_DIR").is_none() {
            assert_eq!(dirs.last(), Some(&bundled_library()));
            let suffix = PathBuf::from(APP).join("workflows");
            assert!(dirs
                .iter()
                .all(|dir| *dir == bundled_library() || dir.ends_with(&suffix)));
            assert!(dirs.len() >= 3, "config, data and system tiers: {dirs:?}");
        }
    }

    /// [`LOAD_ORDER`] is a choice, not a formality: the same two directories
    /// come back in a different order under `LoadOrder::ByName` (ember's and
    /// forge's pick), so this fails if the shim ever stops naming one.
    #[test]
    fn precedence_order_keeps_the_users_directory_first() {
        let user = tempdir();
        let installed = tempdir();
        std::fs::write(user.join("z.yaml"), "name: Zulu\ncommand: echo user\n").unwrap();
        std::fs::write(
            installed.join("a.yaml"),
            "name: Alpha\ncommand: echo installed\n",
        )
        .unwrap();

        let names: Vec<String> = load_library_from(&[user.clone(), installed.clone()])
            .into_iter()
            .map(|workflow| workflow.name)
            .collect();
        assert_eq!(
            names,
            ["Zulu", "Alpha"],
            "precedence order, not alphabetical"
        );

        let _ = std::fs::remove_dir_all(user);
        let _ = std::fs::remove_dir_all(installed);
    }

    /// The bundled-library contract, kept from forge: every example frost
    /// ships must still parse and stay review-only, so an edit to
    /// `scripts/workflows/` that the validator rejects breaks the build rather
    /// than quietly shrinking the picker. The candidate list comes from the
    /// loader's own predicate — re-deriving `toml|yaml|yml` here is exactly
    /// the drift that put a second, unbounded directory walk in anvil.
    #[test]
    fn every_bundled_workflow_is_parseable_and_review_only() {
        let dir = bundled_library();
        let candidates = jterm_core::workflows::workflow_files_in(&dir);
        let workflows = load_library_from(std::slice::from_ref(&dir));
        assert_eq!(workflows.len(), candidates.len());
        assert!(workflows.len() >= 6);
        assert!(workflows
            .iter()
            .all(|workflow| jterm_core::review_input::validate(&workflow.command).is_ok()));
    }

    fn tempdir() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "frost-workflows-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
