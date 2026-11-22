//! `~/.config/trix/instructions.md`: what you tell every agent, after
//! trix's own workspace instructions. Who does what, who reports to whom,
//! what an agent may do without asking. Read as each agent starts, and by
//! each hook that hands an agent its instructions, so an edit reaches the
//! agents started after it; those running keep what they started with.

use std::path::{Path, PathBuf};

/// The most of it that's used: every agent carries it in its prompt.
pub const MAX_BYTES: usize = 16 << 10;

pub fn path() -> PathBuf {
    crate::state::xdg_dir("XDG_CONFIG_HOME", ".config").join("trix/instructions.md")
}

/// Your instructions: none when there's no file or nothing in it, why not
/// when there's one that can't be used.
pub fn user() -> Result<Option<String>, String> {
    // Tests never read the developer's own file.
    if cfg!(test) {
        return Ok(None);
    }
    read(&path())
}

fn read(p: &Path) -> Result<Option<String>, String> {
    let shown = crate::app::tilde(p);
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{shown}: {e}")),
    };
    if bytes.len() > MAX_BYTES {
        return Err(format!("{shown} is {} KB, more than the {} KB agents are given: not used", bytes.len().div_ceil(1024), MAX_BYTES >> 10));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{shown} is not UTF-8 text: not used"))?;
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_string()))
}

/// What every agent is told about the workspace: trix's instructions, then
/// yours. A file that can't be used leaves trix's alone (`user` says why).
pub fn workspace() -> String {
    with(user().ok().flatten().as_deref())
}

fn with(user: Option<&str>) -> String {
    let ours = crate::agent::WORKSPACE_INSTRUCTIONS;
    match user {
        None => ours.to_string(),
        Some(theirs) => format!(
            "{ours}\n\nThe user's own standing instructions for this workspace follow, from {}. Follow them. Where they ask agents to message, report to or hand work to each other, that is the authorization asked for above. They don't lift the rule against --force.\n\n{theirs}",
            crate::app::tilde(&path())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("trix-test-instructions-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn no_file_or_an_empty_one_is_none() {
        let d = dir();
        let p = d.join("instructions.md");
        assert_eq!(read(&p), Ok(None), "no file");
        std::fs::write(&p, "  \n\n\t\n").unwrap();
        assert_eq!(read(&p), Ok(None), "nothing but space");
        std::fs::write(&p, "\n# Roles\n- alpha dog merges\n\n").unwrap();
        assert_eq!(read(&p), Ok(Some("# Roles\n- alpha dog merges".into())), "trimmed");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn too_big_or_not_text_is_not_used() {
        let d = dir();
        let p = d.join("instructions.md");
        std::fs::write(&p, "x".repeat(MAX_BYTES)).unwrap();
        assert!(read(&p).unwrap().is_some(), "the limit itself is fine");
        std::fs::write(&p, "x".repeat(MAX_BYTES + 1)).unwrap();
        assert!(read(&p).unwrap_err().contains("17 KB, more than the 16 KB"));
        std::fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read(&p).unwrap_err().contains("not UTF-8"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn yours_come_after_trix_own_and_are_marked_yours() {
        assert_eq!(with(None), crate::agent::WORKSPACE_INSTRUCTIONS);
        let both = with(Some("alpha dog reviews and merges."));
        assert!(both.starts_with(crate::agent::WORKSPACE_INSTRUCTIONS));
        assert!(both.ends_with("\n\nalpha dog reviews and merges."));
        assert!(both.contains("The user's own standing instructions") && both.contains("instructions.md"));
        assert!(both.contains("They don't lift the rule against --force"));
        assert_eq!(workspace(), crate::agent::WORKSPACE_INSTRUCTIONS, "tests never read the developer's file");
    }
}
