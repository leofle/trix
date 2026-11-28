//! The guard: what every agent pane and every check gets under `push:
//! guarded` and `gate_only`, so an accidental `git push` or `gh` call
//! fails. It narrows the hole and doesn't close it: code that unsets these
//! variables, or reads your credential files itself, isn't stopped.
//! Stopping that would take a sandbox, which trix isn't.

use std::path::{Path, PathBuf};

/// Where every push is sent instead: no remote helper answers to it.
pub const BLOCKED: &str = "trix-guard-blocked://the-gate-pushes-not-you/";

/// The URL beginnings a push can go to, each rewritten to `BLOCKED`.
const PUSH_TO: [&str; 7] = [
    "https://", "http://", "ssh://", "git://", "git@", "file://", "/",
];

/// Variables that would log `gh` in: unset under the guard.
pub const GH_TOKENS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// Variables that tell a process which trix pane it's in: removed from a
/// check, which is no pane's.
pub const TRIX_VARS: [&str; 5] = [
    "TRIX_SESSION",
    "TRIX_AGENT_ID",
    "TRIX_AGENT_NAME",
    "TRIX_SOCKET",
    "TRIX_ROOT",
];

/// An empty folder for the guard to point at: no hooks, no gh login.
pub fn empty_dir() -> PathBuf {
    let dir = crate::worktree::state_dir().join("guard-empty");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The guard's environment: git's config overrides (`GIT_CONFIG_COUNT`
/// and its keys and values) that send every push nowhere and turn the
/// repository's hooks off, and a `gh` with no login.
pub fn env(empty: &Path) -> Vec<(String, String)> {
    let empty = empty.display().to_string();
    let mut config: Vec<(String, String)> = vec![("remote.origin.pushurl".into(), BLOCKED.into())];
    config.extend(
        PUSH_TO
            .iter()
            .map(|p| (format!("url.{BLOCKED}.pushInsteadOf"), p.to_string())),
    );
    // `git commit` in a guarded pane runs no hook the repository carries.
    config.push(("core.hooksPath".into(), empty.clone()));
    let mut out = vec![("GIT_CONFIG_COUNT".to_string(), config.len().to_string())];
    for (i, (k, v)) in config.into_iter().enumerate() {
        out.push((format!("GIT_CONFIG_KEY_{i}"), k));
        out.push((format!("GIT_CONFIG_VALUE_{i}"), v));
    }
    out.push(("GH_CONFIG_DIR".into(), empty));
    out
}

/// The guard for a pane, which is given variables and can't have any
/// taken away: the tokens that would log `gh` in are set to nothing, which
/// `gh` reads as not set.
pub fn pane_env() -> Vec<(String, String)> {
    let mut out = env(&empty_dir());
    out.extend(GH_TOKENS.iter().map(|k| (k.to_string(), String::new())));
    out
}

/// Put the guard on a command about to be started.
pub fn apply(cmd: &mut std::process::Command, empty: &Path) {
    for (k, v) in env(empty) {
        cmd.env(k, v);
    }
    for k in GH_TOKENS {
        cmd.env_remove(k);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
        let mut c = std::process::Command::new("git");
        c.current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        let o = c.output().map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).into())
        }
    }

    /// A repository with one commit and a bare origin beside it.
    pub fn repo(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("trix-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        git(&base, &["init", "-q", "--bare", "origin.git"]).unwrap();
        git(&base, &["init", "-q", "repo"]).unwrap();
        let (repo, origin) = (base.join("repo"), base.join("origin.git"));
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "-A"]).unwrap();
        git(&repo, &["commit", "-q", "-m", "init"]).unwrap();
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        )
        .unwrap();
        git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"]).unwrap();
        (base, repo, origin)
    }

    fn guarded(dir: &Path, empty: &Path, script: &str) -> std::process::Output {
        let mut c = std::process::Command::new("sh");
        c.current_dir(dir)
            .args(["-c", script])
            .env("GH_TOKEN", "ghp_secret")
            .env("GITHUB_TOKEN", "ghp_secret");
        c.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com");
        c.env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com");
        apply(&mut c, empty);
        c.output().unwrap()
    }

    #[test]
    fn under_the_guard_a_push_fails_hooks_dont_run_and_gh_has_no_login() {
        let (base, repo, origin) = repo("env");
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let refs = |o: &Path| git(o, &["for-each-ref", "--format=%(refname)"]).unwrap();
        let before = refs(&origin);
        // A plain push, a forced one, one to the remote's path, and one to a remote added just now.
        for push in [
            "git push origin HEAD:refs/heads/sneak",
            "git push --force origin HEAD:refs/heads/main",
            &format!("git push {} HEAD:refs/heads/sneak", origin.display()),
            &format!(
                "git remote add other file://{} && git push other HEAD:refs/heads/sneak",
                origin.display()
            ),
        ] {
            let o = guarded(&repo, &empty, push);
            assert!(
                !o.status.success(),
                "{push}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        assert_eq!(refs(&origin), before, "origin is as it was");
        // Reading still works: the guard is for pushes.
        assert!(
            guarded(
                &repo,
                &empty,
                "git fetch -q origin && git log -1 --format=%s"
            )
            .status
            .success()
        );
        // A hook the repository carries doesn't run on commit.
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
        std::fs::write(
            repo.join(".git/hooks/pre-commit"),
            "#!/bin/sh\ntouch ../hook-ran\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(
            repo.join(".git/hooks/pre-commit"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let o = guarded(
            &repo,
            &empty,
            "echo b > b.txt && git add b.txt && git commit -q -m b",
        );
        assert!(
            o.status.success() && !base.join("hook-ran").exists(),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        // gh finds no token and no config: the variables that log it in are gone.
        let o = guarded(
            &repo,
            &empty,
            "echo \"${GH_TOKEN:-none}/${GITHUB_TOKEN:-none}/$GH_CONFIG_DIR\"",
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            format!("none/none/{}", empty.display())
        );
        // Without the guard the same push goes through: it's the guard that stops it.
        git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/allowed"]).unwrap();
        assert!(refs(&origin).contains("refs/heads/allowed"));
        let _ = std::fs::remove_dir_all(base);
    }
}
