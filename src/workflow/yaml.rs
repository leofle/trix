//! YAML, for sharing: export a workflow as one self-contained file, import
//! one back. YAML isn't how a workflow is written: the builder owns the
//! native TOML, and import converts.

use super::caps::Matrix;
use super::check::{self, Ctx, Report, Severity};
use super::model::{SCHEMA, Workflow};
use super::store::{Origin, Stores};
use std::path::PathBuf;

/// The format a YAML export says it is.
pub const FORMAT: &str = "trix-workflow/1";

/// A workflow as a self-contained YAML file: a header that says what it
/// is and how it checked here, then the workflow.
pub fn export(w: &Workflow, m: &Matrix, ctx: &Ctx) -> Result<String, String> {
    let report = check::check(w, m, ctx);
    let mut v = serde_norway::to_value(w).map_err(|e| e.to_string())?;
    let obj = v.as_mapping_mut().ok_or("a workflow is a map")?;
    obj.shift_remove("schema");
    // A fork of a built-in is a file of your own.
    obj.shift_remove("builtin");
    // The format first, then the workflow in the order the native file has it.
    let mut doc = serde_norway::Mapping::new();
    doc.insert("format".into(), FORMAT.into());
    doc.extend(std::mem::take(obj));
    let body = serde_norway::to_string(&doc).map_err(|e| e.to_string())?;
    let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let mut head = format!(
        "# Exported by trix. Format {FORMAT}. Edit in the builder\n# (trix workflow edit {}), not here: import converts this file.\n# trix workflow check: {}, {}, on the machine that exported it.\n",
        w.name,
        plural(report.errors(), "error"),
        plural(report.warnings(), "warning")
    );
    for p in report.problems.iter().filter(|p| p.level != Severity::Info) {
        head.push_str(&format!("# {} {}\n", p.level.mark(), p.text));
    }
    Ok(format!("{head}{body}"))
}

/// A YAML file, read into a workflow and checked: the same checker as
/// everywhere. It's never trusted by being imported, and one with errors
/// is a draft that can't run.
pub fn import(text: &str, m: &Matrix, ctx: &Ctx) -> Result<(Workflow, Report), String> {
    let mut v: serde_norway::Value =
        serde_norway::from_str(text).map_err(|e| format!("not YAML trix can read: {e}"))?;
    let obj = v
        .as_mapping_mut()
        .ok_or("the file isn't a workflow: it has no top-level keys")?;
    match obj.shift_remove("format").as_ref().and_then(|f| f.as_str()) {
        Some(FORMAT) => {}
        Some(other) => {
            return Err(format!(
                "format {other} isn't one this trix reads (it reads {FORMAT})"
            ));
        }
        None => {
            return Err(format!(
                "no format: an exported workflow starts with format: {FORMAT}"
            ));
        }
    }
    obj.insert("schema".into(), SCHEMA.into());
    // A file is a file, whatever it says of itself.
    obj.shift_remove("builtin");
    let w: Workflow = serde_norway::from_value(v).map_err(|e| format!("not a workflow: {e}"))?;
    // Through the native format, as any workflow file: its name and keys are held to it.
    let w = Workflow::from_toml(&w.to_toml()?)?;
    // Imported, it isn't trusted: whatever the importer's config says.
    let untrusted = Ctx {
        trusted: Some(false),
        ..ctx.clone()
    };
    let report = check::check(&w, m, &untrusted);
    Ok((w, report))
}

/// Import a file into your own workflows, as native TOML. It never
/// replaces a workflow that's already here: what you trusted under a name
/// stays what you trusted.
pub fn import_into(
    stores: &Stores,
    text: &str,
    m: &Matrix,
    ctx: &Ctx,
) -> Result<(Workflow, Report, PathBuf), String> {
    let (w, report) = import(text, m, ctx)?;
    if stores.load(&w.name).is_ok() || stores.list().iter().any(|(n, _)| *n == w.name) {
        return Err(format!(
            "a workflow named '{}' is already here: give the imported one another name (its name: line), or remove yours first",
            w.name
        ));
    }
    let path = stores.save(&w, Origin::Personal)?;
    Ok((w, report, path))
}

/// What import says of a file: the checker's text, as everywhere, and
/// whether it's a draft.
pub fn import_text(report: &Report) -> String {
    let mut s = report.render();
    if report.errors() > 0 {
        s.push_str("imported as a draft: it can't run until its errors are fixed (trix workflow edit, or check --fix)\n");
    }
    s.push_str(&format!(
        "it isn't trusted yet: read it, then trix workflow trust {}\n",
        report.name
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::store::{builtin, builtin_names};

    fn ideal() -> (Matrix, Ctx) {
        let m = Matrix::shipped();
        let ctx = Ctx::ideal(&m);
        (m, ctx)
    }

    #[test]
    fn native_to_yaml_to_native_is_identical_for_every_template() {
        let (m, ctx) = ideal();
        for name in builtin_names() {
            let mut w = builtin(name).unwrap();
            // A built-in exports as a file of your own.
            w.builtin = false;
            let yaml = export(&w, &m, &ctx).unwrap();
            let (back, _) = import(&yaml, &m, &ctx).unwrap();
            assert_eq!(back, w, "{name}: the same workflow");
            assert_eq!(
                back.to_toml().unwrap(),
                w.to_toml().unwrap(),
                "{name}: and the same native file"
            );
            // And again: exporting what was imported gives the same YAML.
            assert_eq!(export(&back, &m, &ctx).unwrap(), yaml, "{name}");
        }
    }

    #[test]
    fn an_export_is_self_contained_and_says_how_it_checked() {
        let (m, ctx) = ideal();
        let yaml = export(&builtin("issue-to-pr").unwrap(), &m, &ctx).unwrap();
        assert!(yaml.starts_with("# Exported by trix. Format trix-workflow/1. Edit in the builder\n# (trix workflow edit issue-to-pr), not here: import converts this file.\n# trix workflow check: 0 errors, 2 warnings, on the machine that exported it.\n"), "{yaml}");
        assert!(
            yaml.contains("# ⚠ push guarded, not enforced: implementer, judge, tests\n"),
            "{yaml}"
        );
        assert!(
            yaml.contains("\nformat: trix-workflow/1\nname: issue-to-pr\n"),
            "{yaml}"
        );
        // The nodes, briefs, prompts and edges are all there, as YAML.
        for part in [
            "  implementer:\n",
            "      owns:\n      - src/**\n",
            "    kind: check\n",
            "- from: implementer\n",
            "{{fence inputs.issue.body}}",
            "wall_clock: 3h",
        ] {
            assert!(yaml.contains(part), "{part}:\n{yaml}");
        }
        assert!(!yaml.contains("schema:") && !yaml.contains("builtin"));
    }
    #[test]
    fn an_import_lands_in_your_own_workflows_and_replaces_nothing() {
        let (m, ctx) = ideal();
        let dir = std::env::temp_dir().join(format!("trix-yaml-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let stores = Stores {
            team: None,
            personal: dir.join("workflows"),
        };
        let yaml = export(&builtin("issue-to-pr").unwrap(), &m, &ctx).unwrap();
        // Under a built-in's name it's refused: a built-in is read-only.
        let e = import_into(&stores, &yaml, &m, &ctx).unwrap_err();
        assert!(e.contains("already here"), "{e}");
        let mine = yaml.replace("\nname: issue-to-pr\n", "\nname: ours\n");
        let (w, report, path) = import_into(&stores, &mine, &m, &ctx).unwrap();
        assert_eq!((w.name.as_str(), report.errors()), ("ours", 0));
        // Converted: what's kept is the native file, and it reads back the same.
        assert_eq!(path, dir.join("workflows/ours.toml"));
        let (kept, origin) = stores.load("ours").unwrap();
        assert_eq!((kept, origin), (w, Origin::Personal));
        // A second import under the same name doesn't replace the first.
        let other = mine.replace("wall_clock: 3h", "wall_clock: 9h");
        let e = import_into(&stores, &other, &m, &ctx).unwrap_err();
        assert!(e.contains("a workflow named 'ours' is already here"), "{e}");
        assert_eq!(
            stores.load("ours").unwrap().0.limits.wall_clock.as_deref(),
            Some("3h")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_runs_the_checker_and_never_trusts() {
        let (m, ctx) = ideal();
        let w = builtin("issue-to-pr").unwrap();
        let yaml = export(&w, &m, &ctx).unwrap();
        // The same report as everywhere, plus: it isn't trusted.
        let (back, report) = import(&yaml, &m, &ctx).unwrap();
        assert_eq!((report.errors(), report.warnings()), (0, 2));
        assert!(
            !report.runnable() && report.has("untrusted"),
            "imported, it can't run until you trust it"
        );
        let said = import_text(&report);
        assert!(
            said.ends_with("it isn't trusted yet: read it, then trix workflow trust issue-to-pr\n"),
            "{said}"
        );
        assert!(!crate::workflow::policy::is_trusted(
            std::path::Path::new("/nonexistent"),
            &back,
            &crate::workflow::policy::content_hash(&back, &[]),
            false
        ));
        // A file with errors imports as a draft that can't run.
        let broken = yaml.replace("  wall_clock: 3h\n", "");
        let (draft, report) = import(&broken, &m, &ctx).unwrap();
        assert_eq!((draft.limits.wall_clock, report.errors()), (None, 1));
        assert!(
            import_text(&report)
                .contains("imported as a draft: it can't run until its errors are fixed")
        );
        // A file that says it's built in is still a file.
        let liar = yaml.replace(
            "\nname: issue-to-pr\n",
            "\nname: issue-to-pr\nbuiltin: true\n",
        );
        assert!(!import(&liar, &m, &ctx).unwrap().0.builtin);
        // Edited by hand: comments, another order, flow style. Still read.
        let by_hand = "# mine\nname: tiny\nformat: trix-workflow/1\nlimits: { wall_clock: 1h }\ngoal:\n  text: It builds.\n  done_when:\n    mechanical: [{ check: build }]\nnodes:\n  build: { kind: check, run: [make] }\nedges:\n  - { from: build, to: '@done', on: pass }\n  - { from: build, to: '@fail', on: fail }\n";
        let (tiny, report) = import(by_hand, &m, &ctx).unwrap();
        assert_eq!(
            (
                tiny.name.as_str(),
                tiny.nodes["build"].run.clone(),
                report.errors()
            ),
            ("tiny", vec!["make".to_string()], 0)
        );
        // What isn't a workflow is said, not guessed at.
        for (text, why) in [
            ("name: x\n", "no format"),
            (
                "format: trix-workflow/2\nname: x\n",
                "isn't one this trix reads",
            ),
            (
                "format: trix-workflow/1\nname: x\nnodes: {}\ncolour: red\n",
                "not a workflow",
            ),
            (
                "format: trix-workflow/1\nname: ../x\nnodes: {}\n",
                "letters, digits",
            ),
            ("- a\n- b\n", "isn't a workflow"),
            ("{{{", "not YAML"),
        ] {
            let e = import(text, &m, &ctx).unwrap_err();
            assert!(e.contains(why), "{text}: {e}");
        }
    }
}
