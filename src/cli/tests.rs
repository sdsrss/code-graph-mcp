use super::*;

/// Spawning `rg` reports `ErrorKind::NotFound` for two different causes —
/// the binary is missing, or `current_dir` does not exist — and the message
/// named only the first. A user whose indexed project root had been moved was
/// told to install a tool that was already on their PATH.
#[test]
fn rg_spawn_failure_message_distinguishes_missing_cwd_from_missing_binary() {
    let present = tempfile::TempDir::new().unwrap();
    let msg = rg_spawn_failure_message(present.path());
    assert!(
        msg.contains("ripgrep (rg) not found"),
        "an existing root must still point at the binary: {msg}"
    );

    let gone = tempfile::TempDir::new().unwrap();
    let gone_path = gone.path().to_path_buf();
    drop(gone);
    let msg = rg_spawn_failure_message(&gone_path);
    assert!(
        msg.contains("does not exist"),
        "a vanished project root must be named as the cause: {msg}"
    );
    assert!(
        !msg.contains("Install"),
        "must not tell the user to install a tool they have: {msg}"
    );
}

/// M2: `deps` barrel-pattern scanning must not follow a symlink that escapes
/// the project root. The scanner reads a caller-supplied path and echoes its
/// import/export lines; an in-repo symlink to an outside file would turn it
/// into a restricted file-read oracle. Mirrors read_source_context's guard.
#[test]
#[cfg(unix)]
fn scan_barrel_patterns_refuses_symlink_escape() {
    use std::os::unix::fs::symlink;
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    let secret = outside.path().join("secret.ts");
    std::fs::write(
        &secret,
        "import { KEY } from './creds';\nexport { KEY } from './creds';\n",
    )
    .unwrap();
    let link = root.path().join("link.ts");
    symlink(&secret, &link).unwrap();

    assert!(
        scan_barrel_patterns(root.path(), "link.ts").is_none(),
        "scan_barrel_patterns must not follow a symlink escaping the project root",
    );

    // Positive control: an in-root barrel file with the same shape IS scanned.
    std::fs::write(root.path().join("real.ts"), "import { A } from './a';\n").unwrap();
    let ok = scan_barrel_patterns(root.path(), "real.ts");
    assert!(
        ok.as_ref().is_some_and(|h| !h.is_empty()),
        "an in-root barrel file must still be scanned; got: {ok:?}",
    );
}

/// META⑤ drift-guard: `deps` confines file reads to the project root via
/// canonicalize. This asserts an out-of-root symlink yields NO leaked
/// content on EVERY read path `deps` uses for a given file. Step 1 audit
/// (cli.rs cmd_deps, 4945-5111) found `scan_barrel_patterns` is the SOLE
/// content-reader reachable from `deps`: `get_import_tree` only queries the
/// DB, and the MCP twin `tool_dependency_graph`
/// (src/mcp/server/tools/advanced.rs) never reads file contents at all —
/// no barrel fallback exists on the MCP side. `read_source_context`
/// (cli.rs:4626) has the same guard shape but is a `cmd_show`/`cmd_similar`
/// helper, not reachable from `deps` for a given `file_path`, so it is not
/// a `deps` sibling and is out of scope here. This test therefore documents
/// the single-reader finding and pins `scan_barrel_patterns` as that sole
/// path, so the confinement can't be fixed on one path and silently left
/// open on a newly-added sibling later.
/// Negative control: remove the `canonical.starts_with(&root_canonical)`
/// guard in `scan_barrel_patterns` and this returns `Some(...)` → fails.
#[test]
#[cfg(unix)]
fn deps_read_paths_all_refuse_symlink_escape() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let outside = tmp
        .path()
        .parent()
        .unwrap()
        .join("secret-deps-drift-guard.ts");
    std::fs::write(&outside, "import { s } from './secret-source';\n").unwrap();
    let link = root.join("link.ts");
    symlink(&outside, &link).unwrap();

    // Sole read path deps uses for file contents: the barrel-scan fallback.
    // Must refuse the escape and must never surface the outside content.
    let result = scan_barrel_patterns(root, "link.ts");
    assert!(
        result.is_none(),
        "deps' sole content-read path (scan_barrel_patterns) followed a \
             symlink escaping the project root: {result:?}",
    );

    let _ = std::fs::remove_file(&outside);
}

#[test]
fn test_no_embed_flag_parses_on_index_commands() {
    // `--no-embed` is the published fast-path opt-out: structure-first index,
    // skip the slow embedding pass. Verify it wires on every index command and
    // defaults off (embedding stays the default so existing behaviour holds).
    assert!(IncrementalIndexArgs::parse_from(["incremental-index", "--no-embed"]).no_embed);
    assert!(!IncrementalIndexArgs::parse_from(["incremental-index"]).no_embed);
    assert!(ReindexArgs::parse_from(["reindex", "--no-embed"]).no_embed);
    assert!(!ReindexArgs::parse_from(["reindex"]).no_embed);
    assert!(RebuildIndexArgs::parse_from(["rebuild-index", "--confirm", "--no-embed"]).no_embed);
    assert!(!RebuildIndexArgs::parse_from(["rebuild-index", "--confirm"]).no_embed);
}

#[test]
fn test_no_embed_builds_structural_index_without_vectors() {
    // A --no-embed full index must still produce the structural graph (nodes),
    // and must leave zero vectors regardless of model availability — the fast,
    // query-ready state the flag promises.
    use tempfile::TempDir;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("a.rs"), "fn alpha() { beta(); }\nfn beta() {}\n").unwrap();
    let db_path = root.join(CODE_GRAPH_DIR).join("index.db");

    build_full_index_at(&db_path, root, true, true).unwrap();

    let db = Database::open_nondestructive(&db_path).unwrap();
    let nodes: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert!(
        nodes > 0,
        "structure index must be built even with --no-embed"
    );
    let (embedded, _embeddable) = queries::count_nodes_with_vectors(db.conn()).unwrap();
    assert_eq!(embedded, 0, "--no-embed must leave zero vectors");
}

#[test]
fn test_aggregate_recommendations_research_after_answer_and_observe() {
    // Append order = chronological. Each answered deny "arms"; the next
    // grep/read event is a re-search (inline answer didn't end the hunt).
    //   t1 answered deny → t2 grep observe  = re-search
    //   t3 answered deny → t4 cli use       = conversion, NOT re-search
    //   t5 answered deny → t6 read observe  = re-search (read after answer)
    //   t7 UNanswered deny → t8 grep observe = not armed (only answered denies)
    let content = "\
{\"ts\":\"t1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"observe\"}
{\"ts\":\"t3\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t4\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"t5\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t6\",\"hook\":\"read\",\"action\":\"observe\"}
{\"ts\":\"t7\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false}
{\"ts\":\"t8\",\"hook\":\"grep\",\"action\":\"observe\"}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(s.deny_answered, 3, "t1,t3,t5 answered");
    assert_eq!(s.deny_unanswered, 1, "t7 unanswered");
    assert_eq!(
        s.researched_after_answer, 2,
        "t1→t2 (re-grep) and t5→t6 (read) count; t3→t4 (cli use) is a conversion, not re-search"
    );
    // Both follow-ups here are observe (a file read acting on the delivered
    // answer) → neither a sustained drill-down nor a fall-through cg failed.
    assert_eq!(
        s.sustained_after_answer, 0,
        "no follow-up was itself answered by cg"
    );
    assert_eq!(
        s.fallthrough_after_answer, 0,
        "no follow-up was a search cg couldn't satisfy"
    );
    assert_eq!(s.observe, 3, "t2,t6,t8 observes");
    assert_eq!(s.cli_uses, 1, "t4");
    assert_eq!(
        s.by_action.get("observe"),
        None,
        "observe is not a recommendation action"
    );
    assert_eq!(
        s.total, 4,
        "4 denies counted; observe + cli use excluded from total"
    );
}

#[test]
fn test_aggregate_recommendations_followup_split_sustained_vs_fallthrough() {
    // The honest split of "follow-up after an answered deny": cg either
    // answered the next step too (sustained drill-down — a win), the model
    // read a file (observe — acting on the answer), or fell through to a
    // search cg couldn't satisfy (the real insufficiency). `use` between pairs
    // is a clean disarm (conversion, not a search).
    //   L1 answered deny → L2 answered deny       = sustained (cg kept up); L2 re-arms
    //   L2 (armed) → L3 cli use                   = conversion → disarm
    //   L4 answered deny → L5 static deny         = fall-through (cg couldn't); L5 unanswered → no arm
    //   L6 answered deny → L7 grep hint (advisory) = fall-through (no delivered answer)
    //   L8 answered deny → L9 read observe        = neither (acting on answer) → disarm
    //   L10 answered deny → L11 cli use           = conversion → disarm
    //   L12 answered deny → (end)                 = no follow-up (answer sufficed)
    let content = "\
{\"ts\":\"L1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L2\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"mode\":\"grep\"}
{\"ts\":\"L3\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"L4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L5\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false}
{\"ts\":\"L6\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L7\",\"hook\":\"grep\",\"action\":\"hint\"}
{\"ts\":\"L8\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L9\",\"hook\":\"read\",\"action\":\"observe\"}
{\"ts\":\"L10\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L11\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"L12\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(s.deny_answered, 7, "L1,L2,L4,L6,L8,L10,L12 answered");
    assert_eq!(s.deny_unanswered, 1, "L5 static");
    assert_eq!(
        s.researched_after_answer, 4,
        "L2,L5,L7,L9 are search follow-ups; L3/L11 use disarm"
    );
    assert_eq!(
        s.sustained_after_answer, 1,
        "L1→L2: cg answered the follow-up too"
    );
    assert_eq!(
        s.fallthrough_after_answer, 2,
        "L4→L5 (static) and L6→L7 (advisory hint): cg couldn't satisfy"
    );
    assert_eq!(s.observe, 1, "L9");
    assert_eq!(s.cli_uses, 2, "L3,L11");
}

#[test]
fn test_aggregate_recommendations_same_pattern_regrep_is_fallthrough() {
    // Pattern fingerprint tightens `sustained` (the documented upper bound):
    // a verbatim re-grep of the SAME denied pattern after cg answered means
    // the inline answer was ignored/insufficient → fall-through, NOT a
    // drill-down win (and NOT "acting on the answer" even when it lands as a
    // grep observe within the cooldown window). A DIFFERENT pattern is genuine
    // drill-down → sustained. A follow-up WITHOUT a pattern (read observe, or
    // any pre-fix event) keeps the old behavior — back-compatible.
    //   A1 answered deny pattern=foo → arm(foo)
    //   A2 answered deny pattern=foo → SAME (re-deny after cooldown) → fall-through; re-arm(foo)
    //   A3 cli use                   → disarm
    //   A4 answered deny pattern=bar → arm(bar)
    //   A5 grep observe pattern=bar  → SAME (re-grep within cooldown) → fall-through
    //   A6 answered deny pattern=baz → arm(baz)
    //   A7 answered deny pattern=qux → DIFFERENT → sustained; re-arm(qux)
    //   A8 cli use                   → disarm
    //   A9 answered deny pattern=zap → arm(zap)
    //   A10 read observe (no pattern)→ neither (acting on the answer)
    let content = "\
{\"ts\":\"A1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"foo\"}
{\"ts\":\"A2\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"foo\"}
{\"ts\":\"A3\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"A4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"bar\"}
{\"ts\":\"A5\",\"hook\":\"grep\",\"action\":\"observe\",\"pattern\":\"bar\"}
{\"ts\":\"A6\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"baz\"}
{\"ts\":\"A7\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"qux\"}
{\"ts\":\"A8\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"A9\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"zap\"}
{\"ts\":\"A10\",\"hook\":\"read\",\"action\":\"observe\"}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(s.deny_answered, 6, "A1,A2,A4,A6,A7,A9 answered");
    assert_eq!(
        s.researched_after_answer, 4,
        "A2,A5,A7,A10 follow answered denies; A3/A8 use disarm"
    );
    assert_eq!(
        s.fallthrough_after_answer, 2,
        "A1→A2 (same-pattern re-deny) and A4→A5 (same-pattern re-grep observe): answer ignored"
    );
    assert_eq!(
        s.sustained_after_answer, 1,
        "A6→A7: different pattern = genuine drill-down cg also answered"
    );
    assert_eq!(s.observe, 2, "A5,A10");
    assert_eq!(s.cli_uses, 2, "A3,A8");
}

#[test]
fn test_aggregate_recommendations_inconclusive_followup_excluded_from_fallthrough() {
    // Consumer-data over-count fix: a follow-up after an answered deny that is
    // itself a NULL signal about the prior answer must NOT count as fall-through.
    // Two shapes — `no-hits` (cg ran the next grep, found nothing → a NEW query,
    // since a verbatim re-grep of the answered pattern would re-hit it) and
    // `unavailable` (cg CLI couldn't run → infra). Same honesty principle as the
    // v0.64 drill-down/observe exclusion. Same-pattern still wins (verbatim
    // re-grep = answer ignored = real fall-through, even if it now finds nothing).
    //   N1 answered deny → N2 grep hint fallthrough=no-hits          = inconclusive
    //   N3 answered deny → N4 grep deny answered:false reason=unavail = inconclusive
    //   N5 answered deny → N6 grep static deny (answered:false)       = fall-through (cg couldn't)
    //   N7 answered deny pattern=foo → N8 same-pattern deny no-hits   = fall-through (pattern wins)
    let content = "\
{\"ts\":\"N1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"N2\",\"hook\":\"grep\",\"action\":\"hint\",\"fallthrough\":\"no-hits\"}
{\"ts\":\"N3\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"N4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false,\"reason\":\"unavailable\"}
{\"ts\":\"N5\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"N6\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false}
{\"ts\":\"N7\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"foo\"}
{\"ts\":\"N8\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false,\"pattern\":\"foo\",\"fallthrough\":\"no-hits\"}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(s.deny_answered, 4, "N1,N3,N5,N7 answered");
    assert_eq!(
        s.researched_after_answer, 4,
        "N2,N4,N6,N8 all follow answered denies"
    );
    assert_eq!(
        s.followup_inconclusive, 2,
        "N2 (no-hits) + N4 (unavailable): null signal, excluded"
    );
    assert_eq!(
        s.fallthrough_after_answer, 2,
        "N6 (static deny cg couldn't satisfy) + N8 (same-pattern re-grep wins over no-hits)"
    );
    assert_eq!(
        s.sustained_after_answer, 0,
        "no follow-up was itself answered by cg"
    );
}

#[test]
fn test_aggregate_recommendations_inject_arms_and_scores_fallthrough_vs_sustained() {
    // Compound-grep PostToolUse inject: an ANSWERED inject (cg delivered the
    // AST-aware view of a compound-command grep, permission-neutrally) arms the
    // funnel exactly like an answered deny. The immediately-next search event
    // scores the inject's sufficiency, parallel to deny→fallthrough:
    //   I1 inject pattern=foo → arm(foo)
    //   I2 grep observe pattern=foo  → SAME pattern re-grep = inline answer ignored → fall-through; disarm
    //   I3 inject pattern=bar → arm(bar)
    //   I4 grep deny answered=true pattern=qux → DIFFERENT pattern, cg also answered = sustained; re-arm(qux)
    //   I5 cli use → conversion → disarm
    //   I6 inject pattern=baz → arm(baz)
    //   I7 (end) → no follow-up (answer sufficed)
    // inject also counts in total/by_action via the generic map (it is a
    // recommendation event, like deny/hint — NOT observe/use/live_impact).
    let content = "\
{\"ts\":\"I1\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"foo\",\"mode\":\"grep\"}
{\"ts\":\"I2\",\"hook\":\"grep\",\"action\":\"observe\",\"pattern\":\"foo\"}
{\"ts\":\"I3\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"bar\",\"mode\":\"grep\"}
{\"ts\":\"I4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"qux\"}
{\"ts\":\"I5\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"I6\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"baz\",\"mode\":\"grep\"}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(
        s.by_action.get("inject"),
        Some(&3),
        "I1,I3,I6 are inject recommendation events"
    );
    assert_eq!(
        *s.by_hook.get("grep").unwrap(),
        4,
        "I1,I3,I4,I6 are grep recommendation events (I2 observe excluded)"
    );
    assert_eq!(
        s.total, 4,
        "I1,I3,I4,I6 in total; I2 observe + I5 use excluded"
    );
    assert_eq!(
        s.researched_after_answer, 2,
        "I2 (after I1) and I4 (after I3) follow answered injects"
    );
    assert_eq!(
        s.fallthrough_after_answer, 1,
        "I1→I2: same-pattern re-grep = inline inject ignored"
    );
    assert_eq!(
        s.sustained_after_answer, 1,
        "I3→I4: different pattern, cg also answered = drill-down"
    );
    assert_eq!(s.observe, 1, "I2");
    assert_eq!(s.cli_uses, 1, "I5");
}

#[test]
fn test_aggregate_recommendations_inject_by_mode() {
    // The inject payload has a `mode` (callgraph | grep | show). callgraph is the
    // marginal-value cross-file tree; grep/show echo the model's own hits (audit:
    // 0 CONSUMED). `inject_by_mode` breaks the mix down so the callgraph share is
    // directly readable — it is a SUB-breakdown of the inject events, so the total
    // inject count in by_action stays authoritative (not double-counted).
    let content = "\
{\"ts\":\"t1\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"a\",\"mode\":\"callgraph\"}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"b\",\"mode\":\"grep\"}
{\"ts\":\"t3\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"c\",\"mode\":\"callgraph\"}
{\"ts\":\"t4\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"d\",\"mode\":\"show\"}
{\"ts\":\"t5\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"e\"}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(
        s.by_action.get("inject"),
        Some(&5),
        "all 5 are inject events"
    );
    assert_eq!(s.inject_by_mode.get("callgraph"), Some(&2), "t1,t3");
    assert_eq!(s.inject_by_mode.get("grep"), Some(&1), "t2");
    assert_eq!(s.inject_by_mode.get("show"), Some(&1), "t4");
    // t5 has no `mode` field → not counted in any mode bucket; the mode map sums
    // to 4 while by_action.inject stays 5 (mode is best-effort metadata).
    assert_eq!(
        s.inject_by_mode.values().sum::<u64>(),
        4,
        "t5 (no mode) is uncounted"
    );
}

#[test]
fn test_worktree_main_root_boundaries() {
    // Linked worktree (.git FILE, gitdir → …/.git/worktrees/<n>) resolves to
    // the main checkout; a submodule pointer (…/.git/modules/…) and a regular
    // repo (.git DIRECTORY) must both return None (hard boundaries — mirrors
    // project-root.js worktreeMainRoot).
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();

    std::fs::write(
        d.join(".git"),
        format!("gitdir: {}/main/.git/worktrees/wt\n", d.display()),
    )
    .unwrap();
    assert_eq!(worktree_main_root(d), Some(d.join("main")));

    std::fs::write(
        d.join(".git"),
        format!("gitdir: {}/outer/.git/modules/sub\n", d.display()),
    )
    .unwrap();
    assert_eq!(
        worktree_main_root(d),
        None,
        "submodule gitdir is a hard boundary"
    );

    // Separator-agnostic marker (CI windows-latest caught this on v0.100.1):
    // git writes forward slashes in gitdir even on Windows — the first case
    // above already covers that; this one pins the backslash shape, with the
    // returned prefix keeping its ORIGINAL separators.
    std::fs::write(
        d.join(".git"),
        format!("gitdir: {}\\main\\.git\\worktrees\\wt\n", d.display()),
    )
    .unwrap();
    assert_eq!(
        worktree_main_root(d),
        Some(PathBuf::from(format!("{}\\main", d.display()))),
        "backslash gitdir must resolve, preserving native separators"
    );

    std::fs::remove_file(d.join(".git")).unwrap();
    std::fs::create_dir(d.join(".git")).unwrap();
    assert_eq!(worktree_main_root(d), None, ".git DIRECTORY = regular repo");
}

#[test]
fn test_aggregate_recommendations_inject_skipped() {
    // v0.99.1 (roadmap §1.6): the PostToolUse non-hits path now RECORDS
    // (answered:false + fallthrough/reason) instead of staying dark, so the
    // funnel can tell "hook ran, nothing to say" from "hook never fired".
    // answered:false must count in inject_skipped, must NOT arm the funnel,
    // and a skip following an answered deny must score via the existing
    // inconclusive shapes (fallthrough:"no-hits" / reason:"unavailable").
    let content = "\
{\"ts\":\"t1\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"a\",\"fallthrough\":\"no-hits\",\"reason\":\"no-hits\"}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"b\",\"mode\":\"callgraph\"}
{\"ts\":\"t3\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"c\"}
{\"ts\":\"t4\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"d\",\"fallthrough\":\"unavailable\",\"reason\":\"unavailable\"}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(s.inject_skipped, 2, "t1 + t4 are skips");
    assert_eq!(
        s.by_action.get("inject"),
        Some(&3),
        "skips still count as inject events"
    );
    // t1 (answered:false) must NOT arm — so t2 is not scored as a re-search.
    // t2 (answered inject) arms → t3 (answered deny, different pattern) scores
    // sustained; t3 arms → t4's reason:"unavailable" scores inconclusive, not
    // fallthrough. Total re-searches: t2→t3 and t3→t4 (NOT t1→t2).
    assert_eq!(
        s.researched_after_answer, 2,
        "t2→t3 and t3→t4; t1 must not arm"
    );
    assert_eq!(s.sustained_after_answer, 1, "t3 after answered t2");
    assert_eq!(
        s.followup_inconclusive, 1,
        "unavailable skip is a null signal"
    );
    assert_eq!(s.fallthrough_after_answer, 0);
}

#[test]
fn test_inject_mode_buckets_split_delivered_from_skipped() {
    // D#147: the skip path now records `mode` too, so a skipped inject is
    // attributable to the mode that burned the attempt. That field must NOT
    // leak into `inject_by_mode`: stats.rs prints it as "Inject payloads … by
    // mode" and derives the callgraph SHARE from it, and a skipped inject
    // delivered no payload — mixing the two silently redefines the lever
    // metric and makes it non-comparable across versions.
    //
    // The gate is "not explicitly answered:false", not "answered:true":
    // v0.75..v0.99.1 injects carry a `mode` but no `answered` field and were
    // recorded ONLY on hits, so they are delivered and must stay counted.
    let content = "\
{\"ts\":\"t1\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"a\",\"mode\":\"callgraph\"}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"b\",\"fallthrough\":\"no-hits\",\"reason\":\"no-hits\",\"mode\":\"grep\"}
{\"ts\":\"t3\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"c\",\"fallthrough\":\"unavailable\",\"reason\":\"unavailable\",\"mode\":\"grep\"}
{\"ts\":\"t4\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"d\",\"fallthrough\":\"no-hits\",\"reason\":\"no-hits\",\"mode\":\"show\"}
{\"ts\":\"t5\",\"hook\":\"grep\",\"action\":\"inject\",\"pattern\":\"e\",\"mode\":\"callgraph\"}
{\"ts\":\"t6\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"f\",\"fallthrough\":\"no-binary\",\"reason\":\"no-binary\"}
";
    let s = aggregate_recommendations_jsonl(content);

    // Delivered side: t1 (explicit true) + t5 (legacy, no `answered`). The three
    // skips carrying a mode must be absent.
    assert_eq!(
        s.inject_by_mode.get("callgraph"),
        Some(&2),
        "t1 + t5 (legacy no-`answered` injects were delivered)"
    );
    assert_eq!(
        s.inject_by_mode.get("grep"),
        None,
        "t2/t3 are skips — a skipped inject delivered no payload"
    );
    assert_eq!(s.inject_by_mode.get("show"), None, "t4 is a skip");
    assert_eq!(s.inject_by_mode.values().sum::<u64>(), 2);

    // Skipped side: the attribution D#147 asked for.
    assert_eq!(s.inject_skipped_by_mode.get("grep"), Some(&2), "t2 + t3");
    assert_eq!(s.inject_skipped_by_mode.get("show"), Some(&1), "t4");
    assert_eq!(
        s.inject_skipped_by_mode.get("callgraph"),
        None,
        "no skip carried callgraph"
    );
    // t6 is a pre-fix skip (no `mode`) → uncounted in the map but still a skip,
    // so the map sums BELOW inject_skipped exactly like inject_by_mode does
    // against by_action["inject"].
    assert_eq!(s.inject_skipped, 4, "t2,t3,t4,t6");
    assert_eq!(
        s.inject_skipped_by_mode.values().sum::<u64>(),
        3,
        "t6 (no mode) is unattributable — the gap the map must not hide"
    );
    assert_eq!(
        s.by_action.get("inject"),
        Some(&6),
        "all 6 are inject events"
    );
}

#[test]
fn test_aggregate_recommendations_counts_live_impact_separately() {
    // v0.63 — SessionStart live-context injections are a separate counter,
    // like observe/use: NOT in total/by_action, and they don't trip the
    // re-search arming (hook:"session" is not a grep/read search event).
    let content = "\
{\"ts\":\"t1\",\"hook\":\"session\",\"action\":\"live_impact\",\"blast\":72,\"direct\":41,\"wip\":true}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t3\",\"hook\":\"session\",\"action\":\"live_impact\",\"blast\":3,\"direct\":1,\"wip\":false}
{\"ts\":\"t4\",\"hook\":\"grep\",\"action\":\"hint\"}
";
    let s = aggregate_recommendations_jsonl(content);
    assert_eq!(s.live_impact, 2, "t1,t3 live_impact");
    assert_eq!(s.total, 2, "only the deny + hint are recommendation events");
    assert_eq!(
        s.by_action.get("live_impact"),
        None,
        "live_impact is not a recommendation action"
    );
    assert_eq!(
        s.by_hook.get("session"),
        None,
        "session hook is not a recommendation hook"
    );
    // t2 answered deny arms; t3 is live_impact (not a search event) → it must
    // NOT count as a re-search and must disarm.
    assert_eq!(
        s.researched_after_answer, 0,
        "live_impact after an answered deny is not a re-search"
    );
}

#[test]
fn resolve_project_root_prefers_existing_index_at_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let idx_dir = cwd.join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&idx_dir).unwrap();
    std::fs::write(idx_dir.join("index.db"), b"").unwrap();
    assert_eq!(resolve_project_root_from(cwd), cwd);
}

// Helper: give `dir` a `.code-graph/index.db` (explicit join per #937).
fn write_index(dir: &Path) {
    let idx = dir.join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&idx).unwrap();
    std::fs::write(idx.join("index.db"), b"").unwrap();
}

#[test]
fn resolve_project_root_skips_stray_nested_index() {
    // monorepo: root has .git + index; a subdir carries a STRAY index (relic
    // from an older binary) but no .git of its own. Resolving from the subdir
    // must climb to the real root, not pin the stray nested index.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write_index(root);
    let sub = root.join("backend");
    std::fs::create_dir_all(&sub).unwrap();
    write_index(&sub);
    assert_eq!(resolve_project_root_from(&sub), root);
}

#[test]
fn resolve_project_root_stray_index_between_cwd_and_git_root_prefers_git_root() {
    // 3-level monorepo: the real root has .git + index; an INTERMEDIATE subdir
    // carries a STRAY index (no .git of its own); cwd is BELOW that stray. The
    // nearest indexed ancestor is the stray, but the canonical project root is
    // the indexed .git root — resolving must prefer it, matching the JS resolver
    // (project-root.js). Otherwise the CLI reads the stray DB while the hooks
    // read the root DB = split-brain (M7).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write_index(root);
    let mid = root.join("packages").join("app");
    std::fs::create_dir_all(&mid).unwrap();
    write_index(&mid); // stray nested index, no .git
    let cwd = mid.join("src");
    std::fs::create_dir_all(&cwd).unwrap();
    assert_eq!(
        resolve_project_root_from(&cwd),
        root,
        "cwd below a stray nested index must resolve to the indexed .git root, not the stray",
    );
}

#[test]
fn resolve_project_root_nested_index_with_own_git_still_wins() {
    // A real nested repo (submodule / vendored project) has its OWN .git, so
    // its index is legitimate even under an indexed parent — keep it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write_index(root);
    let sub = root.join("vendored");
    std::fs::create_dir_all(sub.join(".git")).unwrap();
    write_index(&sub);
    assert_eq!(resolve_project_root_from(&sub), sub);
}

#[test]
fn resolve_project_root_standalone_index_no_ancestor_still_wins() {
    // No ancestor index → a cwd index is the genuine root (guards against the
    // stray-skip over-reaching into the common single-project case).
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    write_index(cwd);
    assert_eq!(resolve_project_root_from(cwd), cwd);
}

#[test]
fn resolve_project_root_cwd_own_git_no_index_is_boundary() {
    // A fresh project dir with its own `.git` but no index yet (the metrics-
    // isolation fixture) roots at itself, never an indexed ancestor.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_index(root);
    let sub = root.join("pkg");
    std::fs::create_dir_all(sub.join(".git")).unwrap();
    assert_eq!(resolve_project_root_from(&sub), sub);
}

#[test]
fn resolve_project_root_home_boundary_ignores_outer_index() {
    // `~` is both a git repo AND indexed; a project below it with its own
    // index but no `.git` must resolve to itself, not be hijacked to `~`.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".git")).unwrap();
    write_index(home);
    let proj = home.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    write_index(&proj);
    assert_eq!(resolve_project_root_bounded(&proj, Some(home)), proj);
}

#[test]
fn resolve_project_root_non_git_monorepo_prefers_indexed_ancestor() {
    // No `.git` anywhere: a stray subdir index under a non-git indexed root
    // resolves to the indexed ancestor (parity with the JS resolver).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_index(root);
    let sub = root.join("backend");
    std::fs::create_dir_all(&sub).unwrap();
    write_index(&sub);
    // Bound at the tmp parent so the real `~/.code-graph` can't interfere.
    assert_eq!(resolve_project_root_bounded(&sub, root.parent()), root);
}

#[test]
fn resolve_project_root_unindexed_git_root_uses_indexed_mid() {
    // outer/.git (unindexed) / proj/index / backend/stray-index → resolve to
    // the indexed mid dir, not the empty git root.
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
    std::fs::create_dir_all(outer.join(".git")).unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    write_index(&proj);
    let backend = proj.join("backend");
    std::fs::create_dir_all(&backend).unwrap();
    write_index(&backend);
    assert_eq!(resolve_project_root_bounded(&backend, outer.parent()), proj);
}

#[test]
fn test_record_cli_use_rotates_recommendations_jsonl() {
    // record_cli_use is the sole reader of CODE_GRAPH_INTERNAL and no other
    // test mutates it, so toggling it here is race-free.
    std::env::remove_var("CODE_GRAPH_INTERNAL");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let cg = root.join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&cg).unwrap();
    let rec = cg.join("recommendations.jsonl");
    // Pre-fill > 1MB of prior recommendation lines.
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&rec).unwrap();
        let pad = "x".repeat(1024);
        for i in 0..1200 {
            writeln!(f, "{{\"old\":{i},\"pad\":\"{pad}\"}}").unwrap();
        }
    }
    assert!(std::fs::metadata(&rec).unwrap().len() > 1_048_576);

    record_cli_use(root, "callgraph");

    let size = std::fs::metadata(&rec).unwrap().len();
    assert!(
        size < 600_000,
        "recommendations.jsonl should be rotated, got {size} bytes"
    );
    // The freshly recorded use line is last + valid; first surviving line is whole JSON.
    let content = std::fs::read_to_string(&rec).unwrap();
    let last: serde_json::Value =
        serde_json::from_str(content.trim().lines().last().unwrap()).unwrap();
    assert_eq!(last["action"], "use");
    assert_eq!(last["cmd"], "callgraph");
    serde_json::from_str::<serde_json::Value>(content.lines().next().unwrap()).unwrap();
}

#[test]
fn test_record_cli_use_skips_when_no_metrics_sentinel_present() {
    // A `.code-graph/.no-metrics` sentinel silences the recommendations-log
    // writer so a dev/dogfood checkout's own CLI runs (functionality testing,
    // sims, ad-hoc dev) don't self-pollute its adoption metrics with `use`
    // events that read back as genuine consumer traffic. Safe to toggle
    // CODE_GRAPH_INTERNAL (no test SETS it to "1"; parallel removes are idempotent).
    std::env::remove_var("CODE_GRAPH_INTERNAL");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let cg = root.join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&cg).unwrap();
    let rec = cg.join("recommendations.jsonl");

    // No sentinel → the use event is recorded.
    record_cli_use(root, "grep");
    let after_first = std::fs::read_to_string(&rec).unwrap();
    assert_eq!(
        after_first.lines().count(),
        1,
        "use event recorded when no sentinel present"
    );

    // Sentinel present → record_cli_use is a no-op; the file is byte-unchanged.
    std::fs::write(cg.join(crate::domain::NO_METRICS_SENTINEL), b"").unwrap();
    record_cli_use(root, "callgraph");
    let after_second = std::fs::read_to_string(&rec).unwrap();
    assert_eq!(
        after_second, after_first,
        "sentinel must suppress the second use event"
    );
}

#[test]
fn resolve_project_root_climbs_to_git_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let subdir = root.join("sub").join("deep");
    std::fs::create_dir_all(&subdir).unwrap();
    assert_eq!(resolve_project_root_from(&subdir), root);
}

#[test]
fn resolve_project_root_falls_back_to_cwd_when_no_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    // canonicalize both sides: on macOS `/tmp` ↔ `/private/tmp` symlinking;
    // on Linux they match directly, so this is a no-op but keeps the test portable.
    assert_eq!(resolve_project_root_from(cwd), cwd);
}

#[test]
fn is_non_project_cwd_bare_dir_is_non_project() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(is_non_project_cwd(tmp.path()));
}

#[test]
fn is_non_project_cwd_each_marker_makes_it_a_project() {
    for marker in PROJECT_MARKERS {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(marker), b"").unwrap();
        assert!(
            !is_non_project_cwd(tmp.path()),
            "{marker} should classify cwd as a project"
        );
    }
}

#[test]
fn non_project_stub_answers_initialize_tools_list_and_rejects_rest() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"x"}}"#,
        "\n",
    );
    let mut out: Vec<u8> = Vec::new();
    serve_non_project_stub(std::io::Cursor::new(input), &mut out).unwrap();
    let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
    // The notification (no `id`) produces no response → exactly 3 responses.
    assert_eq!(lines.len(), 3, "got: {lines:?}");

    let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        "code-graph-mcp (non-project stub)"
    );

    let tl: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(tl["result"]["tools"], serde_json::json!([]));

    let call: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(call["error"]["code"], -32601);
}

/// CON-05: the stub used a bare `read_line`, whose UTF-8 validation returns
/// `Err(InvalidData)` on a malformed byte — and the `?` carried that straight
/// out of the serve loop, ending the session over one bad request. The full
/// server loop was hardened against exactly this (H3); the stub, which is what
/// every headless `/tmp` session actually gets, was not.
///
/// Negative control: revert `serve_non_project_stub` to `read_line` and this
/// panics at the `unwrap` below, because the call returns Err rather than
/// answering either request.
#[test]
fn non_project_stub_survives_invalid_utf8_line() {
    let mut input: Vec<u8> = Vec::new();
    // A lone 0xFF/0xFE pair is not valid UTF-8 in any position.
    input.extend_from_slice(
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"junk\":\"\xFF\xFE\"}\n",
    );
    input.extend_from_slice(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    input.push(b'\n');

    let mut out: Vec<u8> = Vec::new();
    serve_non_project_stub(std::io::Cursor::new(input), &mut out).unwrap();
    let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();

    // The bad bytes become U+FFFD, so the line is still parseable JSON and gets
    // its normal answer; what matters is that the SECOND request is answered at
    // all — that is the session surviving.
    assert_eq!(
        lines.len(),
        2,
        "session must survive the bad line, got: {lines:?}"
    );
    let ping: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(ping["id"], 1);
    let tl: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(tl["id"], 2);
    assert_eq!(tl["result"]["tools"], serde_json::json!([]));
}

/// CON-05: the stub had no size cap, so one unterminated line allocated without
/// bound. It now mirrors the main loop — reject with -32600, drain the line's
/// tail through its newline, keep serving.
///
/// Negative control: with the old `read_line`, the oversized line is swallowed
/// whole and merely fails to parse, so only ONE response (the tools/list) comes
/// back and the `lines.len() == 2` assert below fires.
#[test]
fn non_project_stub_rejects_oversized_line_and_keeps_serving() {
    use crate::utils::stdio::MAX_MESSAGE_SIZE;

    let mut input: Vec<u8> = Vec::new();
    // One line longer than the cap, terminated: the frame reader must reject it
    // AND consume its tail, or the leftover bytes get misparsed as the next
    // message and the tools/list below is never seen.
    input.extend(std::iter::repeat_n(b'a', MAX_MESSAGE_SIZE + 1024));
    input.push(b'\n');
    input.extend_from_slice(br#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}"#);
    input.push(b'\n');

    let mut out: Vec<u8> = Vec::new();
    serve_non_project_stub(std::io::Cursor::new(input), &mut out).unwrap();
    let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
    assert_eq!(lines.len(), 2, "got: {lines:?}");

    let err: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(err["error"]["code"], -32600);
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Message too large"),
        "got: {}",
        err["error"]["message"]
    );

    let tl: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(tl["id"], 7, "the line after the oversized one must be seen");
}

#[test]
fn cleanup_legacy_db_files_removes_empty_legacy_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Empty legacy files — should be removed
    std::fs::write(dir.join("code-graph.db"), b"").unwrap();
    std::fs::write(dir.join("code_graph.db"), b"").unwrap();
    std::fs::write(dir.join("graph.db"), b"").unwrap();
    // Non-empty legacy file — must NOT be removed (guard against deleting real data)
    std::fs::write(dir.join("index.db"), b"real data").unwrap();
    // Unrelated file — must NOT be touched
    std::fs::write(dir.join("usage.jsonl"), b"").unwrap();

    cleanup_legacy_db_files(dir);

    assert!(!dir.join("code-graph.db").exists());
    assert!(!dir.join("code_graph.db").exists());
    assert!(!dir.join("graph.db").exists());
    assert!(
        dir.join("index.db").exists(),
        "non-empty index.db must survive"
    );
    assert!(
        dir.join("usage.jsonl").exists(),
        "unrelated file must survive"
    );
}

#[test]
fn cleanup_legacy_db_files_keeps_non_empty_legacy() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // If a legacy file has content, it might be a real backup — don't delete.
    std::fs::write(dir.join("graph.db"), b"some content").unwrap();
    cleanup_legacy_db_files(dir);
    assert!(dir.join("graph.db").exists());
}

#[test]
fn resolve_project_root_prefers_cwd_index_over_git_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let subdir = root.join("sub");
    let sub_idx = subdir.join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&sub_idx).unwrap();
    std::fs::write(sub_idx.join("index.db"), b"").unwrap();
    assert_eq!(resolve_project_root_from(&subdir), subdir);
}

// ── Windows path spellings (issue #34) ────────────────────────────────
// Platform-independent on purpose: the bug is pure string handling, so the
// Linux/macOS CI legs catch a regression too, not just windows-latest.

/// Everything `normalize_user_path_from` returns becomes an INDEX LOOKUP
/// KEY, so it must be spelled exactly the way `merkle::normalize_rel_path`
/// stores it. Asserted as a RELATION against the index normalizer rather
/// than a literal, so the same assertion is meaningful on every platform.
///
/// Before the fix, the absolute and root-relative branches returned
/// `to_string_lossy()` verbatim, so on Windows `affected D:\repo\src\Foo.cs`
/// produced the key `src\Foo.cs` against an index holding `src/Foo.cs` and
/// the file was reported "not in index" — a present file, silently dropped.
#[test]
fn normalize_user_path_returns_index_key_spelling() {
    use std::path::PathBuf;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let native_rel: PathBuf = ["src", "parser", "mod.rs"].iter().collect();

    // (1) relative, spelled with the platform's own separator
    let typed = native_rel.to_string_lossy().into_owned();
    let got = normalize_user_path_from(root, root, &typed).unwrap();
    assert_eq!(
        got,
        crate::indexer::merkle::normalize_rel_path(&native_rel),
        "relative input must normalize to the index's stored spelling"
    );

    // (2) the same file given as an absolute path
    let abs = root.join(&native_rel);
    let got_abs = normalize_user_path_from(root, root, &abs.to_string_lossy()).unwrap();
    assert_eq!(
        got_abs, got,
        "absolute and relative spellings of one file must yield ONE key"
    );

    // (3) a backslash-typed relative path — what a Windows user pastes.
    // On Windows that is a two-component path and must become `src/a.rs`;
    // on Unix it is a single legal filename and must survive verbatim.
    let got_bs = normalize_user_path_from(root, root, r"src\a.rs").unwrap();
    assert_eq!(
        got_bs,
        crate::indexer::merkle::normalize_rel_str(r"src\a.rs")
    );
    if cfg!(windows) {
        assert!(
            !got_bs.contains('\\'),
            "no index key may carry a native separator on Windows"
        );
    }
}

/// The Windows legs of `normalize_user_path_from`, executed on every platform
/// via the `backslash_is_sep` seam. Without the parameter these branches were
/// reachable only on the windows-latest CI leg, which is exactly how the `.\`
/// spelling stayed broken: PowerShell tab completion emits `.\src\foo.rs` by
/// default, the `"./"` prefix test never matched it, and the function fell
/// through to produce the key `./src/foo.rs` — a `./`-prefixed key the index
/// never contains (same silent-miss shape as issue #34).
#[test]
fn normalize_user_path_handles_windows_dot_backslash_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // `.\src\foo.rs` must reduce to the bare key, not `./src/foo.rs`.
    let got = normalize_user_path_from_on(root, root, r".\src\foo.rs", true).unwrap();
    assert_eq!(got, "src/foo.rs", "`.\\` is the Windows spelling of `./`");

    // `.` and `.\` both mean "whole project".
    assert_eq!(
        normalize_user_path_from_on(root, root, ".", true).unwrap(),
        ""
    );
    assert_eq!(
        normalize_user_path_from_on(root, root, r".\", true).unwrap(),
        ""
    );

    // Plain backslash relative paths keep working.
    assert_eq!(
        normalize_user_path_from_on(root, root, r"src\foo.rs", true).unwrap(),
        "src/foo.rs"
    );

    // The escape check must still fire through the `.\` prefix — it did not
    // get looser by moving normalization ahead of it.
    assert!(normalize_user_path_from_on(root, root, r".\..\secret", true).is_err());
    assert!(normalize_user_path_from_on(root, root, r"..\secret", true).is_err());

    // Unix leg (`backslash_is_sep = false`): `\` is an ordinary filename
    // character and must survive verbatim — no over-normalization.
    assert_eq!(
        normalize_user_path_from_on(root, root, r"src\foo.rs", false).unwrap(),
        r"src\foo.rs"
    );
}

/// The `_on` seam parameterizes the SEPARATOR but not `Path::is_absolute`,
/// which is irreducibly platform-native. On a Unix host `D:\repo\src\Foo.cs`
/// and `C:/repo/src` are not absolute, so without a lexical guard they fall
/// into the relative branch and come back out as `D:/repo/src/Foo.cs` — a
/// key no index holds, answered as an ordinary empty result.
///
/// The VERDICT is identical on every platform; the MECHANISM is not, and the
/// first version of this guard got that wrong. Off-Windows the lexical
/// spelling check rejects them. ON Windows `is_absolute` recognises the
/// COMPLETE forms natively, `strip_prefix(root)` fails for a foreign drive,
/// and the same error is raised — so the lexical check is not merely
/// redundant for those, it is harmful: applied unconditionally it also
/// rejected `C:\<temp>\src\parser\mod.rs` for a project root that literally
/// contains it, reddening four tests on the windows-latest CI leg while every
/// Linux run stayed green.
///
/// The INCOMPLETE roots are the ones `is_absolute` misses on Windows too —
/// bare `C:` is drive-relative and `\\server` has no share — so they are
/// asserted unconditionally below. The `!cfg!(windows)` version of the guard
/// handed exactly these two back to the relative branch.
#[test]
fn windows_absolute_spellings_are_rejected_by_spelling_off_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for raw in [
        r"D:\repo\src\Foo.cs",
        "C:/repo/src/Foo.cs",
        r"\\server\share\src\Foo.cs",
        "c:/x",
        // Incomplete roots: rejected on EVERY platform, `is_absolute` claims
        // neither anywhere.
        "C:",
        "z:",
        r"\\server",
    ] {
        for backslash_is_sep in [true, false] {
            let got = normalize_user_path_from_on(root, root, raw, backslash_is_sep);
            assert!(
                got.is_err(),
                "{raw:?} (backslash_is_sep={backslash_is_sep}) must be rejected, not \
                     silently turned into an index key; got {got:?}"
            );
        }
    }

    // Near-misses that must NOT be swept up. The first three put a colon at
    // BYTE 1 — the exact position the predicate keys on — because that is
    // the boundary; the earlier version of this control never did, so it
    // passed while the guard rejected the real, indexed root-level file
    // `a:b.rs` with "outside the project root".
    //
    // The colon-bearing cases are Unix-only, and asserting them "on every
    // platform" is what reddened this test on the windows-latest CI leg. `:`
    // is legal in a POSIX filename, so `a:b.rs` there is an ordinary file
    // sitting in the project root and refusing it is a false answer. On
    // Windows it is neither: `a:` is a DRIVE-RELATIVE prefix, and NTFS
    // forbids `:` in a name outright (it introduces an alternate data
    // stream). Rejecting them there is correct, so the expectation, not the
    // behaviour, is what has to be platform-scoped.
    let colon_names: &[&str] = if cfg!(windows) {
        &[]
    } else {
        &["a:b.rs", "z:name", "a:b/c.rs", "src/a:b.rs", "a/b:c"]
    };
    for ok in [&["d/repo/src/Foo.cs"][..], colon_names].concat() {
        for backslash_is_sep in [true, false] {
            assert!(
                normalize_user_path_from_on(root, root, ok, backslash_is_sep).is_ok(),
                "{ok:?} (backslash_is_sep={backslash_is_sep}) is an ordinary \
                     relative path — `:` is legal in a Unix filename — and must survive"
            );
        }
    }
}

/// Repeated separators collapse, because no index key contains `//`.
///
/// `dead-code src// --json` used to answer `[]` exit 0 on a directory with
/// real dead code — the key matched nothing and the empty result was
/// reported as clean. That is the same false-clean the unindexed-path probe
/// exists to prevent: the probe trimmed a TRAILING slash for its own
/// comparison while the query kept the untrimmed filter, so they disagreed
/// and the disclosure never fired. `overview src//` errored on the same
/// input, re-splitting two surfaces that had just been aligned.
///
/// Known limitation, stated rather than hidden: the collapse runs on the
/// OUTPUT (so the `\\`-prefixed UNC rejection still sees its prefix), which
/// means an input whose DOUBLED separator changes how the input itself is
/// parsed is unaffected — `.//src/foo.rs` still errors "escapes the project
/// root", because stripping `./` leaves a leading `/`. Pre-existing, and not
/// reachable from tab completion; `./src//foo.rs` is covered below.
#[test]
fn normalize_user_path_collapses_repeated_separators() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for (raw, want) in [
        ("src//foo.rs", "src/foo.rs"),
        ("src///foo.rs", "src/foo.rs"),
        ("src//", "src/"),
        ("./src//foo.rs", "src/foo.rs"),
        ("a//b//c.rs", "a/b/c.rs"),
        // Single separators are untouched.
        ("src/foo.rs", "src/foo.rs"),
        // A single trailing separator round-trips unchanged, pinned by
        // `test_normalize_user_path_passes_relative_through`; consumers
        // prefix-match, so `src/` works wherever `src` does.
        ("src/parser/", "src/parser/"),
    ] {
        for backslash_is_sep in [true, false] {
            let got = normalize_user_path_from_on(root, root, raw, backslash_is_sep).unwrap();
            assert_eq!(
                got, want,
                "{raw:?} (backslash_is_sep={backslash_is_sep}) must normalize to a key \
                     the index can actually contain"
            );
        }
    }

    // The collapse runs on the OUTPUT, so the UNC rejection still sees its
    // `\\` prefix. Collapsing the input first would turn `\\server\share`
    // into `\server\share` and walk past that guard entirely.
    assert!(
        normalize_user_path_from_on(root, root, r"\\server\share\x.rs", false).is_err(),
        "a UNC root must still be rejected after the collapse was added"
    );
}

/// The WINDOWS branch of that guard, executed on the Linux CI leg.
///
/// `natively_absolute` is the only thing that differs between hosts here, so
/// taking it as a parameter is what makes these cases observable at all —
/// and both earlier versions of the predicate shipped a defect that ONLY
/// windows-latest could see. The test above cannot reach either: it drives
/// the real `Path::is_absolute`, which on Linux answers `false` for every
/// Windows spelling, so the whole "Windows already claimed it" arm is dead
/// there.
#[test]
fn lexical_windows_rejection_only_covers_what_is_absolute_did_not_claim() {
    // Simulated Windows, COMPLETE roots: `is_absolute` claims them, the
    // under-root check is the right answer, and firing here is what reddened
    // four tests on windows-latest by refusing `C:\<root>\src\mod.rs` for a
    // root that literally contains it.
    for complete in [r"C:\repo\src\mod.rs", "C:/repo/src", r"\\srv\share\x"] {
        assert!(
            !needs_lexical_windows_rejection(complete, true),
            "{complete:?} is natively absolute — the lexical check must stand down"
        );
    }
    // Simulated Windows, INCOMPLETE roots: `is_absolute` does NOT claim these
    // there either (bare `C:` is drive-relative, `\\server` has no share), so
    // the `!cfg!(windows)` version let them fall into the relative branch and
    // be answered as an ordinary empty result.
    for incomplete in ["C:", "z:", r"\\server"] {
        assert!(
            needs_lexical_windows_rejection(incomplete, false),
            "{incomplete:?} names no project-relative file on any host and must be rejected"
        );
    }
    // Simulated Unix: nothing here is absolute, so every Windows-shaped form
    // is rejected...
    for shaped in [
        r"D:\repo\src\Foo.cs",
        "C:/repo/src/Foo.cs",
        r"\\server\share\x",
        "c:",
    ] {
        assert!(
            needs_lexical_windows_rejection(shaped, false),
            "{shaped:?} must be rejected lexically where `is_absolute` misses it"
        );
    }
    // ...while ordinary names whose second byte is `:` are not.
    for ok in ["a:b.rs", "z:name", "src/a:b.rs", "d/repo/src/Foo.cs", "1:x"] {
        assert!(
            !needs_lexical_windows_rejection(ok, false),
            "{ok:?} is an ordinary relative path — `:` is legal in a POSIX filename"
        );
    }
}

/// `had_literal_separator` must describe the PATTERN's slot, not the first
/// token that happens to spell the same string.
///
/// `position(|a| *a == pattern)` scanned from index 0, so an earlier flag's
/// VALUE claimed the slot: `grep -t rust -- rust` computed pat=1 against
/// sep=2 and concluded "no separator" for a command that plainly had one.
/// Downstream that makes `grep_flaglike_pattern_hint` advise a user who
/// already typed `--` to type it. The `sep == pat` case (`grep -- --`) was
/// wrong for the same reason, but is unreachable: clap consumes `--` itself,
/// so `grep -- --` never yields a `--` pattern (it exits 2 for a missing
/// PATTERN). Only the value-collision case above is a real command, and it
/// is the one asserted here.
#[test]
fn grep_literal_separator_tracks_the_pattern_slot_not_the_first_match() {
    let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // The pattern string also appears as `-t`'s value, before the separator.
    let a = parse_grep_args(&argv(&[
        "code-graph-mcp",
        "grep",
        "-t",
        "rust",
        "--",
        "rust",
    ]));
    assert_eq!(a.pattern, "rust");
    assert!(
        a.had_literal_separator,
        "the pattern's own slot is after `--`; an earlier flag VALUE spelling \
             the same string must not claim it"
    );

    // Ordinary separated use, and unseparated use, both unchanged.
    assert!(
        parse_grep_args(&argv(&["code-graph-mcp", "grep", "--", "-foo"])).had_literal_separator
    );
    assert!(!parse_grep_args(&argv(&["code-graph-mcp", "grep", "needle"])).had_literal_separator);
    assert!(
        !parse_grep_args(&argv(&["code-graph-mcp", "grep", "-t", "rust", "rust"]))
            .had_literal_separator
    );
}

#[test]
fn is_cwd_anchored_recognizes_both_separator_spellings() {
    // The "cwd-anchored paths never rebase to the project root" promise was
    // Unix-only; on Windows `.\x` fell through and could silently rebase.
    for anchored in [".", "./x", "../x", r".\x", r"..\x"] {
        assert!(is_cwd_anchored(anchored), "{anchored} is cwd-anchored");
    }
    for plain in ["src/x", r"src\x", ".hidden", "x"] {
        assert!(!is_cwd_anchored(plain), "{plain} is not cwd-anchored");
    }
}

#[test]
fn normalize_path_display_strips_windows_prefixes_and_separators() {
    assert_eq!(
        normalize_path_display_on(r"\\?\D:\code\repo\src\main.rs", true),
        "D:/code/repo/src/main.rs",
        r"the \\?\ extended prefix must never reach stdout"
    );
    assert_eq!(
        normalize_path_display_on(r"\\?\UNC\server\share\repo\a.rs", true),
        "//server/share/repo/a.rs",
        "UNC form keeps the double-slash host root"
    );
    assert_eq!(
        normalize_path_display_on("src/main.rs", true),
        "src/main.rs"
    );
    assert_eq!(
        normalize_path_display_on(r"src\main.rs", true),
        "src/main.rs"
    );
}

/// On Unix `\` is a legal FILENAME character (only `/` and NUL are illegal),
/// so normalizing separators there would rename a real file and, worse, build
/// a lookup key that misses the indexed path — `merkle::normalize_rel_path`
/// also rewrites only under `#[cfg(windows)]`. Same failure mode as #34, just
/// in the other direction.
#[test]
fn normalize_path_display_leaves_unix_backslash_filenames_alone() {
    assert_eq!(
        normalize_path_display_on(r"src/od\bc.rs", false),
        r"src/od\bc.rs",
        r"a Unix file literally named `od\bc.rs` must survive verbatim"
    );
    assert_eq!(
        relativize_path_on(r"/home/u/repo/src/od\bc.rs", "/home/u/repo", false),
        r"src/od\bc.rs",
        "…including through relativization, which feeds the AST lookup key"
    );
    // The same input on Windows IS a two-segment path — the flag is the whole
    // difference, which is why it must not be inferred from the string.
    assert_eq!(
        normalize_path_display_on(r"src/od\bc.rs", true),
        "src/od/bc.rs"
    );
}

/// The three spellings one file arrives in on Windows — canonicalized walk
/// output, `project_root.join(rel)` mixed separators, and a bare relative
/// supplement operand — must all reduce to ONE key. When they did not, the
/// same match printed once per spelling and the AST lookup (indexed paths are
/// `/`-relative) missed every file.
#[test]
fn relativize_path_collapses_windows_spellings_to_one_key() {
    let root = r"D:\code\repo";
    let expected = "src/Web/Endpoints.cs";
    for spelling in [
        r"\\?\D:\code\repo\src\Web\Endpoints.cs",
        r"D:\code\repo\src\Web\Endpoints.cs",
        r"D:\code\repo\src/Web/Endpoints.cs",
        "src/Web/Endpoints.cs",
    ] {
        assert_eq!(
            relativize_path_on(spelling, root, true),
            expected,
            "spelling {:?} must relativize to the indexed form",
            spelling
        );
    }
}

#[test]
fn relativize_path_handles_posix_and_dot_walk_output() {
    assert_eq!(
        relativize_path("/home/u/repo/src/main.rs", "/home/u/repo"),
        "src/main.rs"
    );
    assert_eq!(
        relativize_path("/home/u/repo/src/main.rs", "/home/u/repo/"),
        "src/main.rs",
        "a trailing slash on the root must not leave a leading slash"
    );
    assert_eq!(
        relativize_path("./src/main.rs", "/home/u/repo"),
        "src/main.rs"
    );
    // Out-of-root paths can't occur (the starts_with(root) guard rejects
    // them upstream); pinning the pre-existing shape so the normalization
    // rewrite is behaviour-preserving here.
    assert_eq!(
        relativize_path("/elsewhere/x.rs", "/home/u/repo"),
        "elsewhere/x.rs"
    );
}

#[test]
fn relativize_path_is_case_insensitive_on_windows_drive() {
    assert_eq!(
        relativize_path_on(r"d:\code\repo\src\a.rs", r"D:\code\repo", true),
        "src/a.rs",
        "the same volume may be spelled with either drive-letter case"
    );
    // Unix filesystems ARE case-sensitive: two differently-cased roots are
    // two different directories and must not be collapsed.
    assert_eq!(
        relativize_path_on("/home/U/repo/src/a.rs", "/home/u/repo", false),
        "/home/U/repo/src/a.rs".trim_start_matches('/'),
        "case-insensitive matching must stay Windows-only"
    );
}

#[test]
fn test_normalize_type_filter() {
    assert_eq!(normalize_type_filter("fn"), vec!["function", "method"]);
    assert_eq!(normalize_type_filter("class"), vec!["class"]);
    assert_eq!(normalize_type_filter("trait"), vec!["interface", "trait"]);
    assert!(normalize_type_filter("unknown").is_empty());
}

#[test]
fn test_format_node_compact() {
    let node = queries::NodeResult {
        id: 1,
        file_id: 1,
        node_type: "function".into(),
        name: "foo".into(),
        qualified_name: Some("MyClass::foo".into()),
        start_line: 10,
        end_line: 20,
        code_content: String::new(),
        signature: None,
        doc_comment: None,
        context_string: None,
        name_tokens: None,
        return_type: Some("Result<Value>".into()),
        // Mirror how the parser stores it: ALREADY parenthesized. The old fixture
        // used a bare "name: &str, value: i64" (no parens), which never exercised
        // the real shape and hid the "((...))" double-wrap bug.
        param_types: Some("(name: &str, value: i64)".into()),
        is_test: false,
    };
    let formatted = format_node_compact(&node, "src/lib.rs");
    assert!(formatted.contains("fn MyClass::foo"));
    assert!(formatted.contains("src/lib.rs:10-20"));
    assert!(formatted.contains("(name: &str, value: i64)"));
    assert!(formatted.contains("-> Result<Value>"));
    // Guard the fix: param_types already carries its parens, so the formatter must
    // not add a second pair.
    assert!(
        !formatted.contains("(("),
        "must not double-wrap params: {formatted}"
    );
}

#[test]
fn test_parse_rg_json_empty() {
    let root = Path::new("/project");
    assert!(parse_rg_json(b"", root, root).is_empty());
}

#[test]
fn test_parse_rg_json_match() {
    let root = Path::new("/project");
    let json_line = br#"{"type":"match","data":{"path":{"text":"/project/src/main.rs"},"line_number":42,"lines":{"text":"fn main() {\n"}}}"#;
    let matches = parse_rg_json(json_line, root, root);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].file, "src/main.rs");
    assert_eq!(matches[0].line, 42);
}

#[test]
fn test_aggregate_usage_empty() {
    let s = aggregate_usage_jsonl("", None);
    assert_eq!(s.sessions, 0);
    assert_eq!(s.parse_errors, 0);
    assert!(s.tools.is_empty());
    assert_eq!(s.total_tool_calls(), 0);
}

#[test]
fn test_aggregate_usage_skips_malformed_and_blank() {
    let content = "\n\nnot-json\n{\"ts\":\"2026-04-20T00:00:00Z\",\"v\":\"0.12.1\",\"tools\":{}}\n";
    let s = aggregate_usage_jsonl(content, None);
    assert_eq!(s.sessions, 1);
    assert_eq!(s.parse_errors, 1);
}

#[test]
fn test_aggregate_usage_merges_err_kinds_and_tracks_unrecorded_gap() {
    // Shapes mirror metrics.rs `build_record`: err_kinds is an object nested
    // in the tool entry, emitted only when non-empty. Session 3 predates the
    // field (err but no err_kinds) so it must land in the `err`-vs-classified
    // gap, not vanish.
    let s1 = r#"{"ts":"2026-07-01T00:00:00Z","v":"0.90.0","tools":{"get_call_graph":{"n":5,"ms":500,"err":2,"max_ms":200,"err_kinds":{"other":1,"not_found":1}}}}"#;
    let s2 = r#"{"ts":"2026-07-02T00:00:00Z","v":"0.90.0","tools":{"get_call_graph":{"n":3,"ms":300,"err":2,"max_ms":150,"err_kinds":{"other":2}}}}"#;
    let s3 = r#"{"ts":"2026-06-01T00:00:00Z","v":"0.60.0","tools":{"get_call_graph":{"n":1,"ms":90,"err":1,"max_ms":90}}}"#;
    let content = format!("{s1}\n{s2}\n{s3}\n");
    let s = aggregate_usage_jsonl(&content, None);
    let cg = s
        .tools
        .get("get_call_graph")
        .expect("get_call_graph aggregated");
    assert_eq!(cg.n, 9);
    assert_eq!(
        cg.err, 5,
        "err sums across all sessions incl. the pre-err_kinds one"
    );
    assert_eq!(cg.err_kinds.get("other").copied(), Some(3));
    assert_eq!(cg.err_kinds.get("not_found").copied(), Some(1));
    let classified: u64 = cg.err_kinds.values().sum();
    assert_eq!(classified, 4);
    assert_eq!(
        cg.err - classified,
        1,
        "the pre-feature session is the unrecorded remainder"
    );
}

#[test]
fn test_aggregate_usage_merges_tool_counts_across_sessions() {
    let line1 = r#"{"ts":"2026-04-19T10:00:00Z","v":"0.12.0","tools":{"get_call_graph":{"n":2,"ms":200,"err":0,"max_ms":150},"project_map":{"n":1,"ms":1000,"err":0,"max_ms":1000}}}"#;
    let line2 = r#"{"ts":"2026-04-20T10:00:00Z","v":"0.12.1","tools":{"get_call_graph":{"n":3,"ms":900,"err":1,"max_ms":500}}}"#;
    let content = format!("{}\n{}\n", line1, line2);
    let s = aggregate_usage_jsonl(&content, None);
    assert_eq!(s.sessions, 2);
    assert_eq!(s.total_tool_calls(), 6);

    let cg = s.tools.get("get_call_graph").unwrap();
    assert_eq!(cg.n, 5);
    assert_eq!(cg.total_ms, 1100);
    assert_eq!(cg.err, 1);
    assert_eq!(cg.max_ms, 500); // max across sessions

    let pm = s.tools.get("project_map").unwrap();
    assert_eq!(pm.n, 1);
    assert_eq!(pm.max_ms, 1000);

    assert_eq!(s.versions.len(), 2);
    assert!(s.versions.contains("0.12.0") && s.versions.contains("0.12.1"));
    assert_eq!(s.first_ts.as_deref(), Some("2026-04-19T10:00:00Z"));
    assert_eq!(s.last_ts.as_deref(), Some("2026-04-20T10:00:00Z"));
}

#[test]
fn test_aggregate_funnel_deny_and_hint_to_use() {
    // s1: deny + called cg (converted). s2: deny + NO cg (not converted).
    // s3: hint + called cg. s4: no recs (ignored by funnel). s5: deny but only
    // a housekeeping tool (get_index_status) → NOT counted as cg use.
    let s1 = r#"{"ts":"2026-06-10T10:00:00Z","v":"0.45.4","tools":{"get_call_graph":{"n":1,"ms":5,"err":0,"max_ms":5}},"recs":{"deny":2,"hint":0}}"#;
    let s2 = r#"{"ts":"2026-06-10T11:00:00Z","v":"0.45.4","tools":{},"recs":{"deny":1,"hint":1}}"#;
    let s3 = r#"{"ts":"2026-06-10T12:00:00Z","v":"0.45.4","tools":{"find_references":{"n":3,"ms":9,"err":0,"max_ms":4}},"recs":{"deny":0,"hint":1}}"#;
    let s4 = r#"{"ts":"2026-06-10T13:00:00Z","v":"0.45.4","tools":{"get_call_graph":{"n":1,"ms":5,"err":0,"max_ms":5}}}"#;
    let s5 = r#"{"ts":"2026-06-10T14:00:00Z","v":"0.45.4","tools":{"get_index_status":{"n":1,"ms":0,"err":0,"max_ms":0}},"recs":{"deny":1,"hint":0}}"#;
    let content = format!("{s1}\n{s2}\n{s3}\n{s4}\n{s5}\n");
    let s = aggregate_usage_jsonl(&content, None);
    // deny sessions: s1, s2, s5 = 3; of those, only s1 called a cg query tool.
    assert_eq!(s.sessions_with_deny, 3, "s1+s2+s5 saw a deny");
    assert_eq!(
        s.sessions_with_deny_and_cg, 1,
        "only s1 called a cg query tool (s5's get_index_status is housekeeping)"
    );
    // hint sessions: s2, s3 = 2; of those, only s3 called cg.
    assert_eq!(s.sessions_with_hint, 2);
    assert_eq!(s.sessions_with_hint_and_cg, 1);
}

#[test]
fn test_version_sort_key_is_numeric_not_lexical() {
    // Regression: the stats `versions:` list is stored in a BTreeSet (lexical),
    // so "0.5.40" sorted AFTER "0.32.2". version_sort_key must order by numeric
    // (major, minor, patch) so the displayed list reads in true version order.
    let mut vs = vec!["0.32.2", "0.5.40", "0.11.0", "0.9.0", "0.5.43", "0.7.1"];
    vs.sort_by_key(|v| version_sort_key(v));
    assert_eq!(
        vs,
        vec!["0.5.40", "0.5.43", "0.7.1", "0.9.0", "0.11.0", "0.32.2"]
    );
    // Lexical sort would have put "0.11.0"/"0.32.2" before "0.5.40" — guard that.
    assert!(
        vs.iter().position(|v| *v == "0.5.40").unwrap()
            < vs.iter().position(|v| *v == "0.11.0").unwrap(),
        "0.5.40 must sort before 0.11.0 (numeric), not after (lexical)"
    );
    // Odd/suffixed components fall back to 0 without panicking.
    assert_eq!(version_sort_key("0.5.40-rc1"), (0, 5, 40));
    assert_eq!(version_sort_key("weird"), (0, 0, 0));
    assert_eq!(version_sort_key("1.2"), (1, 2, 0));
}

#[test]
fn test_aggregate_usage_last_n_keeps_tail() {
    let lines: Vec<String> = (0..5).map(|i|
            format!(r#"{{"ts":"2026-04-2{}T00:00:00Z","v":"0.12.1","tools":{{"t":{{"n":1,"ms":{},"err":0,"max_ms":{}}}}}}}"#, i, (i + 1) * 10, (i + 1) * 10)
        ).collect();
    let content = lines.join("\n");
    let s = aggregate_usage_jsonl(&content, Some(2));
    assert_eq!(s.sessions, 2);
    let t = s.tools.get("t").unwrap();
    // Last 2 sessions: ms 40 + 50 = 90
    assert_eq!(t.total_ms, 90);
    assert_eq!(t.max_ms, 50);
}

#[test]
fn test_aggregate_recommendations_counts_by_action_and_hook() {
    let content = [
        r#"{"ts":"t1","hook":"grep","action":"deny"}"#,
        r#"{"ts":"t2","hook":"grep","action":"hint"}"#,
        r#"  "#,         // blank → skipped
        r#"{not json}"#, // malformed → skipped, not counted
        r#"{"ts":"t3","hook":"read","action":"hint"}"#,
    ]
    .join("\n");
    let s = aggregate_recommendations_jsonl(&content);
    assert_eq!(s.total, 3, "only 3 well-formed lines counted");
    assert_eq!(s.by_action.get("hint").copied(), Some(2));
    assert_eq!(s.by_action.get("deny").copied(), Some(1));
    assert_eq!(s.by_hook.get("grep").copied(), Some(2));
    assert_eq!(s.by_hook.get("read").copied(), Some(1));
}

#[test]
fn test_aggregate_recommendations_cli_uses_and_answered_split() {
    let content = [
        // answered deny (v0.47+) vs static deny (no field = pre-v0.47 or fallback)
        r#"{"ts":"t1","hook":"grep","action":"deny","answered":true}"#,
        r#"{"ts":"t2","hook":"grep","action":"deny","answered":false}"#,
        r#"{"ts":"t3","hook":"grep","action":"deny"}"#,
        r#"{"ts":"t4","hook":"grep","action":"bypass"}"#,
        // CLI conversions: counted in cli_uses, NOT in total/by_action/by_hook
        r#"{"ts":"t5","hook":"cli","action":"use","cmd":"callgraph"}"#,
        r#"{"ts":"t6","hook":"cli","action":"use","cmd":"grep"}"#,
    ]
    .join("\n");
    let s = aggregate_recommendations_jsonl(&content);
    assert_eq!(s.total, 4, "use lines are conversions, not recommendations");
    assert_eq!(s.cli_uses, 2);
    assert_eq!(s.deny_answered, 1);
    assert_eq!(
        s.deny_unanswered, 2,
        "answered:false and missing field are both static"
    );
    assert_eq!(s.by_action.get("bypass").copied(), Some(1));
    assert!(
        !s.by_hook.contains_key("cli"),
        "cli use lines stay out of by_hook"
    );
}

#[test]
fn test_aggregate_recommendations_empty() {
    let s = aggregate_recommendations_jsonl("");
    assert_eq!(s.total, 0);
    assert!(s.by_action.is_empty());
    assert!(s.by_hook.is_empty());
}

#[test]
fn test_aggregate_usage_search_and_index_merged() {
    let l1 = r#"{"ts":"t1","v":"0.12.1","tools":{"t":{"n":1,"ms":1,"err":0,"max_ms":1}},"search":{"queries":10,"zero":2,"avg_quality":0.8,"fts_only":3,"hybrid":7},"index":{"full_ms":2000,"incr":5,"files":50,"nodes":100}}"#;
    let l2 = r#"{"ts":"t2","v":"0.12.1","tools":{"t":{"n":1,"ms":1,"err":0,"max_ms":1}},"search":{"queries":5,"zero":0,"avg_quality":0.6,"fts_only":1,"hybrid":4},"index":{"full_ms":null,"incr":3,"files":10,"nodes":20}}"#;
    let s = aggregate_usage_jsonl(&format!("{}\n{}", l1, l2), None);
    assert_eq!(s.search_queries, 15);
    assert_eq!(s.search_zero, 2);
    assert_eq!(s.search_fts_only, 4);
    assert_eq!(s.search_hybrid, 11);
    // Weighted quality: (0.8 * 10 + 0.6 * 5) / 15 = 11.0 / 15 ≈ 0.7333
    let weighted_avg = s.search_quality_weighted_sum / s.search_queries as f64;
    assert!((weighted_avg - 0.7333).abs() < 0.01, "got {}", weighted_avg);
    assert_eq!(s.full_index_count, 1);
    assert_eq!(s.full_index_ms_sum, 2000);
    assert_eq!(s.incr_count, 8);
    assert_eq!(s.files_indexed, 60);
}

// --- normalize_user_path ---
// Indexed file_path columns are project-relative; users who paste absolute
// paths from an IDE used to get silent "no results" across overview/deps/dead-code.

#[test]
fn test_normalize_user_path_dot_means_whole_project() {
    // From the project root, `.` is the whole project (empty prefix).
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        normalize_user_path_from(tmp.path(), tmp.path(), ".").unwrap(),
        ""
    );
}

#[test]
fn test_normalize_user_path_strips_dot_slash() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        normalize_user_path_from(tmp.path(), tmp.path(), "./src/parser").unwrap(),
        "src/parser"
    );
}

#[test]
fn test_normalize_user_path_passes_relative_through() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert_eq!(
        normalize_user_path_from(root, root, "src/parser").unwrap(),
        "src/parser"
    );
    assert_eq!(
        normalize_user_path_from(root, root, "src/parser/").unwrap(),
        "src/parser/"
    );
}

#[test]
fn test_normalize_user_path_relative_resolves_against_subdir_cwd() {
    // Running from a subdirectory, a relative path resolves against the cwd
    // (like grep/ls), then maps to a root-relative key. This is the
    // subdir-cwd fix — from `src/`, `main.rs` means `src/main.rs`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let sub = root.join("src");
    let deep = root.join("src/parser");
    // Plain name + `./name` from src/ → src/name.
    assert_eq!(
        normalize_user_path_from(root, &sub, "main.rs").unwrap(),
        "src/main.rs"
    );
    assert_eq!(
        normalize_user_path_from(root, &sub, "./parser").unwrap(),
        "src/parser"
    );
    assert_eq!(
        normalize_user_path_from(root, &sub, "parser/mod.rs").unwrap(),
        "src/parser/mod.rs"
    );
    // `.` from a subdir is that subdir, NOT the whole repo (the bug fixed).
    assert_eq!(normalize_user_path_from(root, &sub, ".").unwrap(), "src");
    assert_eq!(
        normalize_user_path_from(root, &deep, ".").unwrap(),
        "src/parser"
    );
    // `../` climbs back toward the root.
    assert_eq!(
        normalize_user_path_from(root, &deep, "../mod.rs").unwrap(),
        "src/mod.rs"
    );
    assert_eq!(
        normalize_user_path_from(root, &sub, "../Cargo.toml").unwrap(),
        "Cargo.toml"
    );
    // An absolute path is still root-relative regardless of cwd.
    let abs = root.join("lib.rs");
    assert_eq!(
        normalize_user_path_from(root, &sub, abs.to_str().unwrap()).unwrap(),
        "lib.rs"
    );
}

#[test]
fn test_normalize_user_path_root_relative_from_subdir_rebases() {
    // Field failure 2026-07-24 (same class as cmd_grep's): the agent's shell
    // sits in a subdir while it quotes repo-root-relative paths (hook answers
    // display them root-relative), so the cwd-relative reading doubles the
    // prefix into a path that exists nowhere. cwd-missing + root-existing is
    // unambiguous — take the root reading.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let sub = root.join("src");
    std::fs::create_dir_all(sub.join("parser")).unwrap();
    std::fs::write(root.join("src/main.rs"), "").unwrap();
    // File: `src/main.rs` from `src/` — src/src/main.rs missing, src/main.rs exists.
    assert_eq!(
        normalize_user_path_from(root, &sub, "src/main.rs").unwrap(),
        "src/main.rs"
    );
    // Directory: `src` from `src/` — the shape of the observed overview failure.
    assert_eq!(normalize_user_path_from(root, &sub, "src").unwrap(), "src");
    // Parity guard: when the cwd-relative reading exists, it wins — no rebase.
    assert_eq!(
        normalize_user_path_from(root, &sub, "parser").unwrap(),
        "src/parser"
    );
    // Neither exists: historical cwd-relative reading stands (the target may
    // be indexed but deleted/gitignored — do not guess).
    assert_eq!(
        normalize_user_path_from(root, &sub, "ghost.rs").unwrap(),
        "src/ghost.rs"
    );
}

#[test]
fn test_normalize_user_path_rebase_same_name_collision_documented_tradeoff() {
    // Audit 2026-07-24 P2: the rebase heuristic decides by filesystem
    // existence, so when the cwd-relative target was JUST deleted while an
    // unrelated same-named file exists at the root, it picks the root file.
    // Disk state alone cannot distinguish this from the doubling case the
    // heuristic exists for; the stderr note is the disclosure. This test
    // PINS that tradeoff — if it starts failing because the heuristic got
    // smarter (e.g. consults the index), update it deliberately.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // Root-level `utils.rs` exists; `src/utils.rs` (what a user in src/
    // plausibly means) does not — deleted, never existed, or gitignored.
    std::fs::write(root.join("utils.rs"), "").unwrap();
    assert_eq!(
        normalize_user_path_from(root, &root.join("src"), "utils.rs").unwrap(),
        "utils.rs"
    );
}

#[test]
fn test_normalize_user_path_subdir_cwd_rejects_climb_above_root() {
    // From a subdir, enough `../` to climb above the project root is an escape.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let deep = root.join("src/parser");
    // src/parser is depth 2, so escaping the root needs 3+ `../`.
    for escape in ["../../../etc/passwd", "../../../../secret.js"] {
        let err = normalize_user_path_from(root, &deep, escape)
            .expect_err(&format!("{escape:?} from src/parser must escape"));
        assert!(
            format!("{err}").contains("escapes the project root"),
            "got: {err}"
        );
    }
    // Exactly enough `../` to reach the root (not past) is still in-root.
    assert_eq!(
        normalize_user_path_from(root, &deep, "../../lib.rs").unwrap(),
        "lib.rs"
    );
}

#[test]
fn test_normalize_user_path_absolute_under_root_lexical() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let abs = root.join("src/parser");
    assert_eq!(
        normalize_user_path(root, abs.to_str().unwrap()).unwrap(),
        "src/parser"
    );
}

#[test]
fn test_normalize_user_path_rejects_absolute_prefix_climb() {
    // v0.79.1 audit (#5): an ABSOLUTE path that begins with the root then
    // climbs out via `..` (`<root>/../../etc/passwd`) must error. `strip_prefix`
    // matches components and does NOT collapse `..`, so it returned the
    // escaping remainder unchecked — the absolute-prefix sibling of the
    // relative `..` escape. Also covers the `./`-prefix shortcut (`./../x`).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let root_str = root.to_str().unwrap();
    let climbs = [
        format!("{root_str}/../../etc/passwd"),
        format!("{root_str}/../sibling/secret.js"),
        "./../secret".to_string(),
        "./../../etc/passwd".to_string(),
    ];
    for c in &climbs {
        let err = normalize_user_path(root, c)
            .expect_err(&format!("{c:?} must be rejected as an escape"));
        assert!(
            format!("{err}").contains("escapes the project root"),
            "{c:?} should be rejected as an escape; got: {err}"
        );
    }
    // Absolute path under root with interior `..` that stays in-root is allowed.
    let inroot = format!("{root_str}/src/../lib.rs");
    assert_eq!(normalize_user_path(root, &inroot).unwrap(), "src/../lib.rs");
}

#[test]
fn test_db_sidecar_appends_suffix_to_full_filename() {
    // SQLite names the WAL `<dbfile>-wal` — a literal suffix, NOT an extension
    // swap. For `index.db` both happen to agree, but for the rebuild temp
    // `index.db.rebuild-<pid>` only the literal append is correct.
    let canonical = std::path::Path::new("/p/.code-graph/index.db");
    assert_eq!(
        db_sidecar(canonical, "-wal"),
        std::path::PathBuf::from("/p/.code-graph/index.db-wal")
    );
    assert_eq!(
        db_sidecar(canonical, "-shm"),
        std::path::PathBuf::from("/p/.code-graph/index.db-shm")
    );
    let temp = std::path::Path::new("/p/.code-graph/index.db.rebuild-1234");
    assert_eq!(
        db_sidecar(temp, "-wal"),
        std::path::PathBuf::from("/p/.code-graph/index.db.rebuild-1234-wal"),
        "WAL of a multi-dot temp db must append -wal, not swap the extension"
    );
}

#[test]
fn test_normalize_user_path_rejects_relative_dotdot_escape() {
    // A relative path climbing above the root must error, not pass through:
    // the index holds only in-root paths, so an escaping path can only match
    // the disk. `deps`' barrel-scan reads `project_root.join(raw)`, so this is
    // a path-traversal file read (leaks import/re-export lines), not just a
    // wrong query.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for escape in ["../secret.js", "../../etc/passwd", "a/../../b", ".."] {
        let err = normalize_user_path(root, escape).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("escapes the project root"),
            "{escape:?} should be rejected as an escape; got: {msg}"
        );
    }
    // Non-escaping `..` (stays at or below the root) is still allowed through.
    assert_eq!(normalize_user_path(root, "a/../b").unwrap(), "a/../b");
    assert_eq!(
        normalize_user_path(root, "src/sub/../mod.rs").unwrap(),
        "src/sub/../mod.rs"
    );
}

#[test]
fn test_normalize_user_path_absolute_outside_root_errors() {
    let tmp_root = tempfile::tempdir().unwrap();
    let tmp_other = tempfile::tempdir().unwrap();
    let abs_outside = tmp_other.path().join("foo.rs");
    let err = normalize_user_path(tmp_root.path(), abs_outside.to_str().unwrap()).unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("outside the project root"), "got: {}", msg);
}

#[test]
fn test_relativize_path_windows_verbatim_and_case() {
    // cmd_grep hands rg CANONICALIZED search paths; on Windows canonicalize
    // yields the \\?\ verbatim long form while the raw root may be plain
    // (or vice versa when rg echoes verbatim), and drive letters vary in
    // case. The lexical relativize must equate every combination — the
    // remaining 8.3-short-name vs long-name pairing is filesystem-only and
    // is covered by cmd_grep relativizing against root_canonical (the same
    // canonical spelling the rg args were built from), exercised by the
    // grep e2e suite on the windows CI leg (first lit in v0.112.0).
    assert_eq!(
        relativize_path_on(
            r"C:\Users\dev\proj\src\a.rs",
            r"\\?\C:\Users\dev\proj",
            true
        ),
        "src/a.rs"
    );
    assert_eq!(
        relativize_path_on(
            r"\\?\C:\Users\dev\proj\src\a.rs",
            r"C:\Users\dev\proj",
            true
        ),
        "src/a.rs"
    );
    assert_eq!(
        relativize_path_on(r"c:\proj\x.rs", r"C:\proj", true),
        "x.rs"
    );
}

#[test]
fn test_relativize_path_dual_falls_back_to_raw_root() {
    // A nonexistent path never canonicalizes, so its rg echo keeps the RAW
    // (possibly 8.3-short) root spelling; the canonical long form misses
    // lexically and the raw fallback must strip it instead. Unmatched by
    // both roots → normalized passthrough.
    assert_eq!(
        relativize_path_dual("/repo/src/a.rs", "/repo", "/elsewhere"),
        "src/a.rs"
    );
    assert_eq!(
        relativize_path_dual("/short/form/src/a.rs", "/canonical/long", "/short/form"),
        "src/a.rs",
        "canonical miss must fall back to the raw root"
    );
    assert_eq!(
        relativize_path_dual("/outside/a.rs", "/repo", "/elsewhere"),
        // Unmatched passthrough keeps relativize_path's historical shape:
        // normalized with the leading separator trimmed.
        "outside/a.rs"
    );
}

#[test]
fn test_normalize_user_path_absolute_under_root_canonicalize_via_symlink() {
    // Symlink case: lexical strip fails but canonicalize succeeds.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src/parser")).unwrap();
    let link_root = tmp
        .path()
        .parent()
        .unwrap()
        .join(format!("cg-norm-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link_root);
    #[cfg(unix)]
    std::os::unix::fs::symlink(root, &link_root).unwrap();
    #[cfg(unix)]
    {
        let abs_via_link = link_root.join("src/parser");
        let res = normalize_user_path(root, abs_via_link.to_str().unwrap()).unwrap();
        assert_eq!(res, "src/parser");
        let _ = std::fs::remove_file(&link_root);
    }
}

#[test]
fn test_normalize_grep_argv_attached_context() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // Attached numeric context forms split into flag + value.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-A2", "pat"])),
        s(&["grep", "-A", "2", "pat"])
    );
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-B1", "pat"])),
        s(&["grep", "-B", "1", "pat"])
    );
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-C10", "pat"])),
        s(&["grep", "-C", "10", "pat"])
    );
    // Bundled boolean short(s) + trailing attached context: peel the digits,
    // keep the cluster so clap parses `-nA 2` as `-n -A=2`.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-nA2", "pat"])),
        s(&["grep", "-nA", "2", "pat"])
    );
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-niB3", "pat"])),
        s(&["grep", "-niB", "3", "pat"])
    );
    // Value flag not last in the bundle (`-A2B3`) → digit in the middle → left alone.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-A2B3"])),
        s(&["grep", "-A2B3"])
    );
    // Bare `-A` (clap takes the next token as its value) is untouched.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-A", "2", "pat"])),
        s(&["grep", "-A", "2", "pat"])
    );
    // Non-context single-dash flags and `--long` patterns are untouched.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-n", "pat"])),
        s(&["grep", "-n", "pat"])
    );
    assert_eq!(
        normalize_grep_argv(s(&["grep", "--no-default-features"])),
        s(&["grep", "--no-default-features"])
    );
    // `-m` is the `--max-count` short alias: attached `-m2` splits like `-A2`
    // (the same allow_hyphen_values quirk forces the peel — see the fn doc).
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-m2", "pat"])),
        s(&["grep", "-m", "2", "pat"])
    );
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-nm2", "pat"])),
        s(&["grep", "-nm", "2", "pat"])
    );
    // `-M` (`--max-columns`) is also a numeric value short → attached splits.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-M512", "pat"])),
        s(&["grep", "-M", "512", "pat"])
    );
    // Digit-suffix on an unsupported short (`-z2`) is left alone.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-z2", "pat"])),
        s(&["grep", "-z2", "pat"])
    );
    // Non-digit tail (`-A2x`) is not a valid attached form → left alone.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "-A2x"])),
        s(&["grep", "-A2x"])
    );
    // `--` stops normalization so a literal `-A2` pattern survives.
    assert_eq!(
        normalize_grep_argv(s(&["grep", "--", "-A2"])),
        s(&["grep", "--", "-A2"])
    );
}

#[test]
fn test_first_unsupported_grep_flag() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // Common grep flags we don't implement are flagged (would otherwise be
    // swallowed as the pattern → cryptic "No such file").
    for bad in ["-v", "-o", "-e", "-P", "-x", "-nv"] {
        assert_eq!(
            first_unsupported_grep_flag(&s(&["grep", bad, "pat"])).as_deref(),
            Some(bad),
            "{bad} should be reported as unsupported"
        );
    }
    // Supported shorts (incl. bundles + attached/standalone value shorts) pass.
    // -c (count), -t (type), -g (glob), -M (max-columns) were added in v0.79.
    for ok in [
        "-i", "-w", "-F", "-l", "-n", "-r", "-R", "-H", "-A2", "-nA2", "-niB3", "-C", "-m", "-m5",
        "-iw", "-c", "-t", "-g", "-M", "-M512",
    ] {
        assert_eq!(
            first_unsupported_grep_flag(&s(&["grep", ok, "pat"])),
            None,
            "{ok} is supported, must not be flagged"
        );
    }
    // Dash-then-symbol/digit terms are searchable patterns, not flags.
    for pat in ["->", "-1", "-.*", "-->foo"] {
        assert_eq!(
            first_unsupported_grep_flag(&s(&["grep", pat])),
            None,
            "{pat} is a pattern, must not be flagged"
        );
    }
    // `--` escapes a literal flag-shaped term.
    assert_eq!(first_unsupported_grep_flag(&s(&["grep", "--", "-v"])), None);
    // Unsupported flag after a supported value short's value is still caught.
    assert_eq!(
        first_unsupported_grep_flag(&s(&["grep", "-A", "2", "-v", "pat"])).as_deref(),
        Some("-v")
    );
}

/// A LONG flag this subcommand does not implement is deliberately left to the
/// pattern positional (so `grep --no-default-features` stays searchable), and
/// the real pattern then becomes a path. That behavior is a published CLI
/// contract and is unchanged; what is new is that the resulting rg error gets
/// explained instead of standing alone as `rg: <your pattern>: No such file`.
#[test]
fn grep_flaglike_pattern_hint_fires_only_on_the_confusing_pairing() {
    const MISSING: &str = "rg: /repo/alpha: No such file or directory (os error 2)";

    // The confusing case: flag-shaped pattern AND a missing-path complaint.
    for flag in ["--quiet", "--json", "--check-only", "--no_color", "--x1"] {
        let hint = grep_flaglike_pattern_hint(flag, MISSING, false)
            .unwrap_or_else(|| panic!("{flag} + missing-path must be explained"));
        assert!(hint.contains(flag), "hint must name the token: {hint}");
        assert!(
            hint.contains("grep -- "),
            "hint must give the escape that keeps the literal search working: {hint}"
        );
    }

    // Same pattern, a DIFFERENT rg failure: not this diagnosis, stay quiet.
    assert_eq!(
        grep_flaglike_pattern_hint("--quiet", "rg: regex parse error", false),
        None
    );

    // An explicit `--` means the caller already said they meant the literal.
    // Their missing path is their own typo (`grep -- --no-default-features
    // src/nope`), and the hint would tell them to add the separator they
    // just typed.
    assert_eq!(
        grep_flaglike_pattern_hint("--no-default-features", MISSING, true),
        None,
        "an explicit `--` must silence the hint entirely"
    );

    // A genuine literal search that happens to fail on a missing path must not
    // be told it typed a flag — these are ordinary patterns, not `--word`.
    for pat in ["alpha", "-v", "->", "-1", "-.*", "--", "--=x", "--a b"] {
        assert_eq!(
            grep_flaglike_pattern_hint(pat, MISSING, false),
            None,
            "{pat} is not a long-flag spelling and must not be explained as one"
        );
    }
}

// --- rebuild-index / reindex concurrency gate (audit 2026-08-02 P1-3) ---

/// A project dir with one source file and an index already built, plus a
/// HELD `index.lock`. The guard file is returned: flock is released when it
/// drops, so the caller must keep it alive for the length of the assertion.
#[cfg(unix)]
fn locked_project() -> (tempfile::TempDir, std::fs::File) {
    use std::os::unix::io::AsRawFd;
    let project = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(
        project.path().join("src/a.ts"),
        "export function alpha(): number { return 1; }\n",
    )
    .unwrap();
    let cg = project.path().join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&cg).unwrap();
    // Built through the CLI's own indexer entry point rather than a raw
    // `Database::open`: the reader_nondestructive drift guard scans this
    // whole file (test module included) for the destructive constructor, and
    // it is right to — the fixture does not need an exemption when the real
    // command builds the same index.
    cmd_incremental_index(project.path(), true, true).unwrap();

    // flock is held per OPEN FILE DESCRIPTION, not per process: a second
    // `open()` of the same path — which is exactly what
    // `other_process_holds_index_lock` does — conflicts with this one even
    // from inside this same test process. That makes the "another server is
    // running" state reproducible without spawning anything.
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(cg.join("index.lock"))
        .unwrap();
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "test fixture failed to take the index lock");
    assert!(
        crate::indexer::lock::other_process_holds_index_lock(&cg),
        "fixture precondition: the probe must SEE the held lock, else every \
             assertion below passes vacuously"
    );
    (project, lock)
}

/// The P1-3 defect: `rebuild-index` warned and then renamed a fresh index
/// over the one a running MCP server still had open, stranding that server's
/// writes on a deleted inode. Refusal is the default now.
#[cfg(unix)]
#[test]
fn rebuild_index_refuses_while_another_process_holds_the_index_lock() {
    let (project, _lock) = locked_project();
    let before = std::fs::metadata(project.path().join(CODE_GRAPH_DIR).join("index.db"))
        .unwrap()
        .len();

    let err = cmd_rebuild_index(
        project.path(),
        RebuildIndexArgs {
            confirm: true,
            quiet: false,
            no_embed: true,
            force: false,
            json: false,
        },
    )
    .expect_err("a held index lock must block the rename");
    let msg = err.to_string();
    assert!(
        msg.contains("holds the index lock"),
        "the refusal must name the cause: {msg}"
    );
    assert!(
        msg.contains("--force"),
        "the refusal must name the escape hatch: {msg}"
    );
    // The refusal happens BEFORE any mutation — the live index is untouched.
    let after = std::fs::metadata(project.path().join(CODE_GRAPH_DIR).join("index.db"))
        .unwrap()
        .len();
    assert_eq!(before, after, "a refused rebuild must not touch index.db");
}

/// `--quiet` used to `return` out of `warn_if_index_locked` before the probe
/// ran, so the noisiest-path caller (hooks) was the one with NO protection.
/// quiet governs printing only.
#[cfg(unix)]
#[test]
fn rebuild_index_quiet_still_detects_the_lock() {
    let (project, _lock) = locked_project();
    let err = cmd_rebuild_index(
        project.path(),
        RebuildIndexArgs {
            confirm: true,
            quiet: true,
            no_embed: true,
            force: false,
            json: false,
        },
    )
    .expect_err("--quiet must silence output, not the check");
    assert!(
        err.to_string().contains("holds the index lock"),
        "got: {err}"
    );
}

/// The escape hatch has to actually work, or users with a defunct lock file
/// are stuck with no way to rebuild.
#[cfg(unix)]
#[test]
fn rebuild_index_force_proceeds_despite_the_lock() {
    let (project, _lock) = locked_project();
    cmd_rebuild_index(
        project.path(),
        RebuildIndexArgs {
            confirm: true,
            quiet: true,
            no_embed: true,
            force: true,
            json: false,
        },
    )
    .expect("--force must override the refusal");
    let db = Database::open_nondestructive(&project.path().join(CODE_GRAPH_DIR).join("index.db"))
        .unwrap();
    let nodes: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert!(nodes > 0, "the forced rebuild must produce a usable index");
}

/// Sibling path: `reindex --from-snapshot` unlinks index.db outright, which
/// strands the same open fd. It had no warning at all before this.
#[cfg(unix)]
#[test]
fn reindex_from_snapshot_refuses_while_locked_and_keeps_the_index() {
    let (project, _lock) = locked_project();
    let db_path = project.path().join(CODE_GRAPH_DIR).join("index.db");

    let err = cmd_reindex(
        project.path(),
        ReindexArgs {
            from_snapshot: true,
            no_embed: true,
            force: false,
            json: false,
        },
    )
    .expect_err("a held index lock must block the unlink");
    assert!(
        err.to_string().contains("holds the index lock"),
        "got: {err}"
    );
    assert!(
        db_path.exists(),
        "the refusal must come BEFORE the remove_file, or the damage is done anyway"
    );
}

/// Negative control: with no lock held, the same call goes through. Without
/// it, a guard that refused unconditionally would pass every test above.
#[cfg(unix)]
#[test]
fn rebuild_index_proceeds_when_no_one_holds_the_lock() {
    let (project, lock) = locked_project();
    drop(lock); // release
    assert!(
        !crate::indexer::lock::other_process_holds_index_lock(&project.path().join(CODE_GRAPH_DIR)),
        "control precondition: the lock must read as free once released"
    );
    cmd_rebuild_index(
        project.path(),
        RebuildIndexArgs {
            confirm: true,
            quiet: true,
            no_embed: true,
            force: false,
            json: false,
        },
    )
    .expect("an unlocked index must rebuild without --force");
}

/// The gate has to EXCLUDE, not just observe. While it only probed, two
/// `rebuild-index --confirm` runs both read the lock as free, both entered
/// the `index.db.rebuild-*` temp sweep — which deletes any other run's
/// in-progress temp on purpose — and the loser died with a bare SQLite
/// `disk I/O error` (QA ISSUE-008).
#[cfg(unix)]
#[test]
fn lock_index_for_replace_claims_the_lock_it_reports_free() {
    let (project, lock) = locked_project();
    drop(lock);
    let cg = project.path().join(CODE_GRAPH_DIR);
    assert!(
        !crate::indexer::lock::other_process_holds_index_lock(&cg),
        "precondition: the lock must start free, else the claim below is unobservable"
    );

    let guard = lock_index_for_replace(&cg, false).unwrap();
    assert!(
        guard.is_some(),
        "a free lock must be CLAIMED, not merely observed to be free"
    );
    assert!(
        crate::indexer::lock::other_process_holds_index_lock(&cg),
        "while the guard is alive the lock must read as HELD — that is the whole \
             difference between excluding a concurrent rebuild and warning about a server"
    );

    drop(guard);
    assert!(
        !crate::indexer::lock::other_process_holds_index_lock(&cg),
        "the guard must release on drop, or one rebuild would poison every later run"
    );
}

/// Acquire / held / release, asserted identically on both platforms.
///
/// It used to be platform-ASYMMETRIC, and the asymmetry was the bug. On unix
/// the flock dies with the handle and the lock FILE is kept on purpose —
/// deleting it would hand a concurrent holder's lock to a different inode. The
/// Windows leg used to say the opposite: the file's existence WAS the lock, so
/// the guard deleted it on drop, and `other_process_holds_index_lock` answered
/// from a recorded PID it excluded when it matched our own — a lock this very
/// process held read as free there. Both of those are gone: the Windows lock is
/// now an exclusive open handle, so every assertion below holds verbatim on
/// both platforms and none of them needs a `cfg`.
///
/// That is the point of this test. The Windows arm cannot run on the dev host,
/// so the fewer of its lines that are `cfg`-gated away here, the more of it CI's
/// windows leg actually executes.
#[test]
fn index_lock_guard_releases_on_drop_on_this_platform() {
    // Deliberately does NOT use `locked_project()` — that fixture is unix-only
    // (it takes a raw flock), and the arm this test exists for is the Windows
    // one.
    let project = tempfile::TempDir::new().unwrap();
    let cg = project.path().join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&cg).unwrap();
    assert!(
        !crate::indexer::lock::other_process_holds_index_lock(&cg),
        "precondition: a fresh project has no lock"
    );

    let guard = crate::indexer::lock::acquire_index_lock_guard(&cg)
        .expect("a free lock must be acquirable");
    assert!(
        cg.join("index.lock").exists(),
        "precondition: the guard must have created the lock file"
    );
    assert!(
        crate::indexer::lock::other_process_holds_index_lock(&cg),
        "a held lock must read as held — a second open in this same process \
             conflicts on both platforms, which is what the CLI gate relies on"
    );
    drop(guard);

    assert!(
        !crate::indexer::lock::other_process_holds_index_lock(&cg),
        "the lock must read as FREE after the guard drops, or one CLI rebuild \
             poisons every later run on this machine"
    );
    assert!(
        cg.join("index.lock").exists(),
        "the file is kept on both platforms; removing it would break mutual \
             exclusion with a concurrent holder"
    );
}

/// The third arm: the lock is not HELD, it simply cannot be opened. That arm
/// decides a rebuild proceeds UNLOCKED, so a wrong verdict here either bricks
/// a rebuild that used to work (if it refused) or silently drops the
/// exclusion. Reached with a read-only `.code-graph/`, which is also why the
/// asymmetry exists: refusing would make this gate the reason the command
/// stopped working, on a directory whose permissions have nothing to do with
/// concurrency.
#[cfg(unix)]
#[test]
fn lock_index_for_replace_proceeds_when_the_lock_file_cannot_be_opened() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: root ignores directory permissions");
        return;
    }
    let (project, lock) = locked_project();
    drop(lock);
    let cg = project.path().join(CODE_GRAPH_DIR);
    std::fs::remove_file(cg.join("index.lock")).unwrap();
    let original = std::fs::metadata(&cg).unwrap().permissions();
    std::fs::set_permissions(&cg, std::fs::Permissions::from_mode(0o555)).unwrap();

    let outcome = lock_index_for_replace(&cg, false);

    // Restore before asserting so a failure still leaves a removable TempDir.
    std::fs::set_permissions(&cg, original).unwrap();
    let guard = outcome.expect("an unopenable lock file must not refuse the replace");
    assert!(
        guard.is_none(),
        "nothing was locked, so no guard can be handed back"
    );
}

/// End of the same story, at the command level: with rebuild #1's guard held,
/// rebuild #2 gets the explanatory refusal instead of a SQLite error, and the
/// refusal names the concurrent-rebuild case (the old text blamed only "a
/// running MCP server", which was never true here).
#[cfg(unix)]
#[test]
fn a_second_rebuild_refuses_while_the_first_holds_the_lock() {
    let (project, lock) = locked_project();
    drop(lock);
    let cg = project.path().join(CODE_GRAPH_DIR);
    // Stand-in for rebuild #1: the exact guard cmd_rebuild_index now keeps
    // alive for the length of its run.
    let _first = lock_index_for_replace(&cg, false)
        .unwrap()
        .expect("rebuild #1 must get the lock");

    let err = cmd_rebuild_index(
        project.path(),
        RebuildIndexArgs {
            confirm: true,
            quiet: true,
            no_embed: true,
            force: false,
            json: false,
        },
    )
    .expect_err("rebuild #2 must refuse while #1 holds the lock");
    let msg = err.to_string();
    assert!(
        msg.contains("holds the index lock"),
        "the refusal must name the cause: {msg}"
    );
    assert!(
        msg.contains("rebuild-index"),
        "the refusal must cover the concurrent-CLI case, not just the MCP server: {msg}"
    );
}

/// P2 (2026-08-16 audit §四): `show --impact` and `impact` report a RISK level for
/// the same symbol, and they disagreed — `show` passed a literal confidence rank
/// of 0 (keep the ambiguous by-name fan-out) while `impact` defaults to the
/// `inferred` floor. Two risk levels for one symbol, and neither output carries a
/// field that would explain it.
///
/// Pinning `show`'s constant to the shared default means the next change to the
/// floor cannot move one surface without the other.
#[test]
fn impact_and_show_agree_on_the_default_confidence_floor() {
    use crate::cli::commands::show::SHOW_IMPACT_MIN_CONF_RANK;
    let impact_default = crate::domain::confidence_rank(
        crate::domain::parse_min_confidence(None, "--min-confidence")
            .unwrap()
            .unwrap_or(crate::domain::DEFAULT_RISK_CONF_FLOOR),
    );
    assert_eq!(
        SHOW_IMPACT_MIN_CONF_RANK, impact_default,
        "show --impact must use the same caller-traversal floor as `impact`"
    );
    // And the floor must actually exclude something, or the agreement is vacuous:
    // rank 0 (ambiguous) would make both surfaces keep every by-name edge.
    assert!(
        impact_default > crate::domain::confidence_rank(crate::domain::CONF_AMBIGUOUS),
        "the default floor must sit above `ambiguous`, or it filters nothing"
    );
}

/// The `""` half of the same cluster: `--min-confidence ""` is how a shell spells
/// an unset variable, and it used to be the default on `callgraph`/`impact` and a
/// hard error on `refs` — same flag, same input, opposite outcomes. `parse_min_confidence`
/// is now the single reading, and empty means absent everywhere.
#[test]
fn empty_min_confidence_reads_as_absent_on_every_surface() {
    use crate::domain::parse_min_confidence;
    assert_eq!(
        parse_min_confidence(None, "--min-confidence").unwrap(),
        None
    );
    assert_eq!(
        parse_min_confidence(Some(""), "--min-confidence").unwrap(),
        None,
        "an empty value must mean 'not given', not an error"
    );
    assert_eq!(
        parse_min_confidence(Some("extracted"), "--min-confidence").unwrap(),
        Some(crate::domain::CONF_EXTRACTED)
    );
    // Still strict about real typos — this must not become an accept-everything.
    let err = parse_min_confidence(Some("inferrd"), "--min-confidence").unwrap_err();
    assert!(
        err.to_string().contains("--min-confidence must be one of"),
        "a typo must still fail, with the surface's own flag spelling: {err}"
    );
    let mcp_err = parse_min_confidence(Some("nope"), "min_confidence").unwrap_err();
    assert!(
        mcp_err.to_string().starts_with("min_confidence must be"),
        "the MCP surface keeps its own spelling: {mcp_err}"
    );
}

/// `.code-graph/recommendations.jsonl` is opened by name and appended to. A
/// clone that ships a symlink there redirected every CLI query's telemetry line
/// into an unrelated file (audit 2026-08-29 SEC-02 — the same primitive that
/// truncates once the target crosses the 1 MB rotation threshold).
#[cfg(unix)]
#[test]
fn record_cli_use_refuses_to_append_through_a_symlink() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let cg = root.join(crate::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&cg).unwrap();
    let victim = dir.path().join("victim.conf");
    std::fs::write(&victim, "keep = 1\n").unwrap();
    std::os::unix::fs::symlink(&victim, cg.join("recommendations.jsonl")).unwrap();

    record_cli_use(&root, "search");

    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "keep = 1\n",
        "the link target must not be appended to"
    );

    // Positive control in the SAME process, so an inherited `CODE_GRAPH_INTERNAL=1`
    // (which early-returns) cannot make the assertion above vacuously green.
    let plain_root = dir.path().join("plain");
    let plain_cg = plain_root.join(crate::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&plain_cg).unwrap();
    record_cli_use(&plain_root, "search");
    let written = std::fs::read_to_string(plain_cg.join("recommendations.jsonl")).unwrap();
    assert!(
        written.contains("\"cmd\":\"search\""),
        "a regular recommendations.jsonl must still receive the line: {written:?}"
    );
}

/// The whole data directory is repo-suppliable: `.code-graph -> ../outside`
/// made `create_dir_all` a silent no-op and put `index.db` (and every telemetry
/// file with it) outside the project root (audit 2026-08-29 SEC-03).
#[cfg(unix)]
#[test]
fn a_symlinked_code_graph_dir_is_refused_before_anything_is_written() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(crate::domain::CODE_GRAPH_DIR)).unwrap();

    let db_path = root.join(crate::domain::CODE_GRAPH_DIR).join("index.db");
    let err = match build_full_index_at(&db_path, &root, true, true) {
        Err(e) => e,
        Ok(_) => panic!("a symlinked .code-graph must be refused"),
    };
    assert!(
        err.to_string().contains(".code-graph"),
        "the refusal must name the directory it refuses: {err}"
    );
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "nothing may be written outside the project root"
    );
}

/// The shape the first SEC-03 pass missed, and the pre-tag reviewer caught:
/// `O_NOFOLLOW` and `refuse_non_regular` judge only the FINAL path component, so
/// a symlinked `.code-graph` holding perfectly ORDINARY files defeats both — the
/// write lands on a real regular file that simply is not where the caller thinks
/// it is. The original PoC linked `.code-graph/index.lock` itself (final
/// component a symlink, which O_NOFOLLOW does catch) and so proved the wrong
/// half.
///
/// Measured before the fix: a 54-byte config at `outside/index.lock` became 1
/// byte (a PID digit) and `outside/index.db` was DELETED — and the refusal still
/// printed afterwards, because every guard sat downstream of the destruction.
#[cfg(unix)]
#[test]
fn destructive_commands_refuse_a_symlinked_data_dir_before_touching_anything() {
    for cmd in ["reindex", "rebuild-index"] {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        // Ordinary files, NOT symlinks — that is the whole point.
        const KEEP: &str = "IMPORTANT CONFIG LINE 1\nLINE 2 with more content here\n";
        std::fs::write(outside.join("index.lock"), KEEP).unwrap();
        std::fs::write(outside.join("index.db"), "not really a db").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(crate::domain::CODE_GRAPH_DIR)).unwrap();

        let err = match cmd {
            "reindex" => cmd_reindex(
                &root,
                crate::cli::commands::ReindexArgs {
                    from_snapshot: true,
                    no_embed: true,
                    force: false,
                    json: false,
                },
            ),
            _ => cmd_rebuild_index(
                &root,
                crate::cli::RebuildIndexArgs {
                    confirm: true,
                    quiet: true,
                    no_embed: true,
                    force: false,
                    json: false,
                },
            ),
        }
        .expect_err("{cmd} must refuse a symlinked .code-graph");
        assert!(
            err.to_string().contains("symlinked"),
            "{cmd}: refusal must name the reason: {err}"
        );

        assert_eq!(
            std::fs::read_to_string(outside.join("index.lock")).unwrap(),
            KEEP,
            "{cmd} wrote a PID through the directory symlink"
        );
        assert!(
            outside.join("index.db").exists(),
            "{cmd} deleted a file outside the project root"
        );
    }
}

/// CLI half of the `9b4821c` regression (the `indexer` half lives in
/// `indexer::lock::tests::a_held_lock_is_still_seen_through_a_hardlinked_lock_file`,
/// which cannot call into `crate::cli` — `tests/hardening.rs` forbids that edge).
///
/// `9b4821c` applied the hardlink refusal to `probe_owned`, which never writes.
/// `other_process_holds_index_lock` reads any open error as "free", so a
/// hardlinked `index.lock` — what `cp -al` / `rsync --link-dest` leave behind —
/// made a HELD lock read as free, and this function fell through to its
/// "proceeding unlocked" arm and replaced an index a live process was writing.
#[cfg(unix)]
#[test]
fn replace_refuses_when_a_held_lock_file_has_a_hardlink() {
    use std::os::unix::io::AsRawFd;
    let dir = tempfile::TempDir::new().unwrap();
    let cg = dir.path().join(".code-graph");
    std::fs::create_dir_all(&cg).unwrap();
    let lock = cg.join("index.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::hard_link(&lock, dir.path().join("backup.lock")).unwrap();

    // Stand in for the live server holding the lock.
    let holder = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
    // SAFETY: `holder` is an open File owned by this scope, so the fd is live.
    let rc = unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "the test holder must be able to take the lock");

    // `match`, not `expect_err`: the Ok side is `Option<IndexLockGuard>`, and the
    // guard holds a live File rather than deriving Debug.
    let err = match crate::cli::index_ops::lock_index_for_replace(&cg, false) {
        Ok(_) => panic!("a held lock must refuse the replace, not proceed unlocked"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("holds the index lock"),
        "the refusal must name the reason: {err}"
    );

    // Negative control: released, the same hardlinked lock file stops refusing —
    // so the assertion above is about the holder, not about a call that always
    // errors.
    drop(holder);
    assert!(
        crate::cli::index_ops::lock_index_for_replace(&cg, false).is_ok(),
        "with no holder the replace must be allowed to proceed"
    );
}

/// `read_source_context` returns the range its text actually covers, and that
/// range is what `content_start_line` / `content_end_line` publish.
///
/// This shipped in `0.134.0` with zero tests (pre-ship review, HIGH #2): the
/// value comes from a `saturating_sub` and two `as i64` casts, and the whole
/// point of the field is that a caller TRUSTS it to map `code_content` line 1
/// onto a file line. Every edge is pinned here — both clamps and the middle —
/// by asserting the returned first/last line numbers index the returned text's
/// own first/last lines back to the real file.
#[test]
fn read_source_context_reports_the_range_it_actually_covers() {
    use crate::cli::commands::show::read_source_context;
    let root = tempfile::TempDir::new().unwrap();
    // 10 numbered lines: line N reads "L{N}".
    let body: String = (1..=10).map(|i| format!("L{i}\n")).collect();
    std::fs::write(root.path().join("f.rs"), &body).unwrap();
    let lines: Vec<&str> = body.lines().collect();

    let check = |start: i64, end: i64, ctx: usize, want: (i64, i64)| {
        let (text, first, last) = read_source_context(root.path(), "f.rs", start, end, ctx)
            .unwrap_or_else(|| panic!("no content for {start}-{end} ±{ctx}"));
        assert_eq!(
            (first, last),
            want,
            "range for symbol {start}-{end} ±{ctx}; text was:\n{text}"
        );
        let got: Vec<&str> = text.lines().collect();
        assert_eq!(
            got.len() as i64,
            last - first + 1,
            "line count must equal the reported span: {got:?}"
        );
        assert_eq!(got[0], lines[(first - 1) as usize], "first line mismatch");
        assert_eq!(
            *got.last().unwrap(),
            lines[(last - 1) as usize],
            "last line mismatch"
        );
    };

    // Mid-file: context available on both sides, so the range widens by `ctx`.
    check(5, 6, 2, (3, 8));
    // Leading clamp: a symbol at line 1 cannot grow upward past it.
    check(1, 2, 3, (1, 5));
    // Trailing clamp: a symbol at EOF cannot grow downward past it.
    check(9, 10, 3, (6, 10));
    // Both clamps at once — asking for more context than the file has.
    check(4, 5, 100, (1, 10));
    // ctx = 0: the range IS the symbol, which is what makes the two fields
    // omittable (and keeps a `context_lines: 0` response byte-identical).
    check(4, 6, 0, (4, 6));
}
