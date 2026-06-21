//! WP-DF-IGNORE matcher unit tests: pattern parse (glob, `**`, `!`, comments,
//! blanks), the per-path exclusion verdict, and the control-file always-out
//! rule. Pure and parallel-safe (no filesystem, no process-global state).
use super::DockerIgnore;

#[test]
fn star_glob_excludes_matching_extension() {
    let di = DockerIgnore::parse("*.log\n");
    assert!(di.is_excluded("app.log"));
    assert!(di.is_excluded("error.log"));
    assert!(!di.is_excluded("app.txt"));
}

#[test]
fn bang_reincludes_after_exclude_last_match_wins() {
    // `*.log` then `!keep.log`: keep.log is re-included, others stay excluded.
    let di = DockerIgnore::parse("*.log\n!keep.log\n");
    assert!(di.is_excluded("app.log"));
    assert!(!di.is_excluded("keep.log"));
}

#[test]
fn reinclude_then_reexclude_last_wins() {
    // Order matters: a later exclude overrides an earlier re-include.
    let di = DockerIgnore::parse("!keep.log\n*.log\n");
    assert!(di.is_excluded("keep.log"));
}

#[test]
fn trailing_slash_dir_excludes_dir_and_contents() {
    let di = DockerIgnore::parse("node_modules/\n");
    assert!(di.is_excluded("node_modules"));
    assert!(di.is_excluded("node_modules/pkg/index.js"));
    assert!(!di.is_excluded("src/index.js"));
}

#[test]
fn plain_dir_name_excludes_recursively() {
    let di = DockerIgnore::parse("build\n");
    assert!(di.is_excluded("build"));
    assert!(di.is_excluded("build/out/a.o"));
    assert!(!di.is_excluded("buildscript.sh"));
}

#[test]
fn comments_and_blank_lines_ignored() {
    let di = DockerIgnore::parse("# a comment\n\n   \n*.tmp\n# trailing\n");
    assert!(di.is_excluded("x.tmp"));
    assert!(!di.is_excluded("x.keep"));
}

#[test]
fn doublestar_matches_any_depth() {
    let di = DockerIgnore::parse("**/*.log\n");
    assert!(di.is_excluded("a.log"));
    assert!(di.is_excluded("deep/nested/dir/a.log"));
    assert!(!di.is_excluded("deep/nested/a.txt"));
}

#[test]
fn doublestar_suffix_matches_subtree() {
    let di = DockerIgnore::parse("logs/**\n");
    assert!(di.is_excluded("logs/a.log"));
    assert!(di.is_excluded("logs/2024/01/a.log"));
    // `logs/**` requires at least... moby treats `logs/**` as logs subtree; the
    // dir itself is also excluded via the directory rule.
    assert!(di.is_excluded("logs"));
}

#[test]
fn question_mark_matches_one_char() {
    let di = DockerIgnore::parse("file?.c\n");
    assert!(di.is_excluded("file1.c"));
    assert!(di.is_excluded("fileA.c"));
    assert!(!di.is_excluded("file.c"));
    assert!(!di.is_excluded("file12.c"));
}

#[test]
fn nested_path_pattern_matches_exact_subpath() {
    let di = DockerIgnore::parse("src/secret.txt\n");
    assert!(di.is_excluded("src/secret.txt"));
    assert!(!di.is_excluded("secret.txt"));
    assert!(!di.is_excluded("src/public.txt"));
}

#[test]
fn present_but_empty_file_is_active_excludes_no_user_paths() {
    // A PRESENT-but-empty `.dockerignore` is ACTIVE (the control-file rule fires)
    // but excludes no USER path — only the always-out Dockerfile/.dockerignore.
    let di = DockerIgnore::parse("");
    assert!(!di.is_inactive(), "a present .dockerignore is active");
    assert!(!di.is_excluded("anything"));
    assert!(!di.is_excluded("a/b/c"));
}

#[test]
fn absent_file_is_inactive() {
    // `DockerIgnore::default()` models NO `.dockerignore` present: inactive, so
    // even the control-file rule does NOT fire (byte-identical to pre-WP).
    let di = DockerIgnore::default();
    assert!(di.is_inactive(), "no .dockerignore ⇒ inactive");
    assert!(
        !di.is_excluded("Dockerfile"),
        "inactive ⇒ no control-file rule"
    );
    assert!(!di.is_excluded("anything"));
}

#[test]
fn dockerignore_and_dockerfile_always_excluded() {
    // Even with NO user patterns, the two control files are kept out of COPY .
    let di = DockerIgnore::parse("");
    assert!(di.is_excluded(".dockerignore"));
    assert!(di.is_excluded("Dockerfile"));
    // Path normalization: a leading `./` still hits the control-file rule.
    assert!(di.is_excluded("./Dockerfile"));
}

#[test]
fn control_files_excluded_even_if_user_reincludes() {
    // A `!Dockerfile` exception cannot re-include the Dockerfile (Docker's rule).
    let di = DockerIgnore::parse("!Dockerfile\n!.dockerignore\n");
    assert!(di.is_excluded("Dockerfile"));
    assert!(di.is_excluded(".dockerignore"));
}

#[test]
fn leading_dot_slash_normalized() {
    let di = DockerIgnore::parse("*.log\n");
    assert!(di.is_excluded("./app.log"));
}
