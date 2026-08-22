//! Effectiveness benchmark — keeps the response-size win a regression-tracked
//! number rather than an impression.
//!
//! For each navigation task in the corpus, runs the equivalent code-graph CLI
//! command on a fixture project and compares the byte count of the response
//! to a hardcoded `baseline_bytes` representing the historical Grep+Read
//! approach. Both numbers are logged; the assertion fires only if the overall
//! code-graph/baseline ratio exceeds 0.60 — i.e. if more than 60% of the
//! baseline bytes survive. (The README once carried a "40-60% session token
//! savings" headline this ceiling was named after; that line is gone, and the
//! README now sources its numbers from this bench instead.)
//!
//! Bytes are a token proxy; for English / TS source they correlate ~1:3 with
//! BPE tokens (byte ≈ 0.33 token), so a 50% byte reduction maps to a 50%
//! token reduction with the same ratio. The harness intentionally avoids a
//! tokenizer dependency — bytes-as-proxy is good enough for tracking trend
//! over releases.
//!
//! ## Running
//!
//! ```bash
//! # Build the binary first (test uses CARGO_BIN_EXE_code-graph-mcp).
//! cargo build --no-default-features
//! cargo test --test effectiveness_bench --no-default-features -- --ignored --nocapture
//! ```
//!
//! ## Adding tasks
//!
//! Each task has a `baseline_bytes` estimate of the Grep+Read approach. Set
//! it once by hand-counting (or by running grep/Read for the same intent and
//! summing the bytes touched), then commit. A regression in code-graph
//! response size will move the ratio without touching the baseline. To
//! re-baseline after a justified blow-up, update the constant inline and
//! reference the CHANGELOG entry that explains why.
//!
//! ## Why ignored by default
//!
//! Spawning the binary takes 80-150ms per task × 5 tasks = ~750ms; small
//! enough to run, but pollutes the regular `cargo test` suite when iterating.
//! CI cron / release-tag jobs run with `--ignored`.

use std::process::Command;
use tempfile::TempDir;

fn binary_path() -> String {
    env!("CARGO_BIN_EXE_code-graph-mcp").to_string()
}

/// Single benchmark task: a CLI invocation + a baseline byte estimate of the
/// Grep+Read approach to the same intent.
struct Task {
    name: &'static str,
    args: &'static [&'static str],
    /// Estimated bytes a "without code-graph" agent would read. Order-of-
    /// magnitude is enough — used for ratio computation, not absolute claim.
    baseline_bytes: usize,
    /// Allowed worst-case ratio (code_graph_bytes / baseline_bytes). Per-task
    /// because some intents legitimately need more bytes than others.
    max_ratio: f64,
}

const CORPUS: &[Task] = &[
    // Project map: agent without code-graph would `ls -R src/` + read 5+ top files.
    // Estimate: ls ~500 bytes + 5 × 1500 byte file reads = ~8000 bytes.
    Task {
        name: "project_map",
        args: &["map", "--compact"],
        baseline_bytes: 8000,
        max_ratio: 0.50,
    },
    // Module overview: read every file in a 4-file dir.
    // Estimate: 4 × 1000 bytes = ~4000 bytes.
    Task {
        name: "module_overview src/auth/",
        args: &["overview", "src/"],
        baseline_bytes: 4000,
        max_ratio: 0.60,
    },
    // Callers of a function: grep "validateToken(" + read ~3 caller sites.
    // Estimate: grep ~500 bytes + 3 × 800 bytes = ~3000 bytes.
    Task {
        name: "callers of validateToken",
        args: &[
            "callgraph",
            "validateToken",
            "--direction",
            "callers",
            "--depth",
            "2",
        ],
        baseline_bytes: 3000,
        max_ratio: 0.60,
    },
    // ast_search by return type: grep ": Result<" + read every match.
    // Estimate: grep + read ~5 sites = ~3000 bytes.
    Task {
        name: "ast_search returns boolean",
        args: &["ast-search", "--returns", "boolean"],
        baseline_bytes: 3000,
        max_ratio: 0.60,
    },
    // Find references: grep + classify by relation type (calls / imports / etc).
    // Estimate: grep + read 5 import + 3 caller sites = ~5000 bytes.
    Task {
        name: "find_references validateToken",
        args: &["refs", "validateToken"],
        baseline_bytes: 5000,
        max_ratio: 0.60,
    },
];

/// Build an indexed fixture project mirroring cli_e2e's setup pattern.
/// Realistic enough that the CLI commands return non-empty output.
fn setup_indexed_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(
        src.join("auth.ts"),
        r#"
import jwt from 'jsonwebtoken';

export function validateToken(token: string): boolean {
    const decoded = jwt.verify(token, process.env.SECRET);
    return decoded !== null;
}

export function hashPassword(password: string): string {
    return password;
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("api.ts"),
        r#"
import { validateToken } from './auth';

export function handleLogin(req: Request, res: Response) {
    const user = validateToken(req.headers.authorization);
    if (!user) { res.status(401); return; }
    res.json({ userId: user.id });
}

export function handleLogout(req: Request, res: Response) {
    res.json({ ok: true });
}

export function checkAuth(token: string): boolean {
    return validateToken(token);
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("utils.ts"),
        r#"
export function formatDate(date: Date): string {
    return date.toISOString();
}

export class Logger {
    log(msg: string) {
        console.log(msg);
    }
}

export function isValid(input: unknown): boolean {
    return input !== null && input !== undefined;
}
"#,
    )
    .unwrap();

    // Index using the library directly (avoids spawn overhead during setup).
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    project
}

#[derive(Debug)]
struct TaskResult {
    name: &'static str,
    code_graph_bytes: usize,
    baseline_bytes: usize,
    ratio: f64,
    max_ratio: f64,
    passed: bool,
}

fn run_task(project_root: &std::path::Path, task: &Task) -> TaskResult {
    let bin = binary_path();
    let output = Command::new(&bin)
        .args(task.args)
        .current_dir(project_root)
        .env("CODE_GRAPH_QUIET_HOOKS", "1") // silence any plugin-side hooks
        .output()
        .unwrap_or_else(|e| panic!("spawn {} {:?}: {}", bin, task.args, e));

    if !output.status.success() {
        // Don't panic — record as a failure with bytes=0 so the report shows it.
        eprintln!(
            "[effectiveness_bench] task '{}' exited {:?}: {}",
            task.name,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }

    let bytes = output.stdout.len();
    let ratio = bytes as f64 / task.baseline_bytes as f64;
    TaskResult {
        name: task.name,
        code_graph_bytes: bytes,
        baseline_bytes: task.baseline_bytes,
        ratio,
        max_ratio: task.max_ratio,
        passed: ratio <= task.max_ratio,
    }
}

#[test]
#[ignore = "spawns the binary multiple times; run: cargo test --test effectiveness_bench -- --ignored --nocapture"]
fn effectiveness_against_grep_read_baseline() {
    let project = setup_indexed_project();
    let project_root = project.path();

    eprintln!(
        "[effectiveness_bench] project={} corpus_size={}",
        project_root.display(),
        CORPUS.len(),
    );

    let results: Vec<TaskResult> = CORPUS.iter().map(|t| run_task(project_root, t)).collect();

    // Per-task report.
    eprintln!("\n=== Per-task token savings (bytes-as-proxy) ===");
    eprintln!(
        "{:<40}  {:>10}  {:>10}  {:>8}  {:>8}",
        "task", "cg_bytes", "baseline", "ratio", "max",
    );
    for r in &results {
        let marker = if r.passed { "PASS" } else { "FAIL" };
        eprintln!(
            "{:<40}  {:>10}  {:>10}  {:>7.2}x  {:>7.2}x  [{}]",
            r.name, r.code_graph_bytes, r.baseline_bytes, r.ratio, r.max_ratio, marker,
        );
    }

    // Aggregate report.
    let total_cg: usize = results.iter().map(|r| r.code_graph_bytes).sum();
    let total_baseline: usize = results.iter().map(|r| r.baseline_bytes).sum();
    let overall_ratio = total_cg as f64 / total_baseline as f64;
    eprintln!(
        "\nOverall: {} cg_bytes / {} baseline = {:.2}x (savings = {:.0}%)",
        total_cg,
        total_baseline,
        overall_ratio,
        (1.0 - overall_ratio) * 100.0,
    );

    // Per-task assertions are accumulated, then asserted once so all rows print.
    let failures: Vec<String> = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| format!("{} ({:.2}x > {:.2}x)", r.name, r.ratio, r.max_ratio))
        .collect();
    assert!(
        failures.is_empty(),
        "[effectiveness_bench] {} task(s) exceeded their max_ratio: {}",
        failures.len(),
        failures.join(", "),
    );

    // Headline assertion: overall ratio must beat the README's worst-case
    // claim (40% savings → 0.60 ratio). This is what gets quoted in PR
    // reviews and CHANGELOG release headers.
    const HEADLINE_MAX_RATIO: f64 = 0.60;
    assert!(
        overall_ratio <= HEADLINE_MAX_RATIO,
        "Overall ratio {:.2}x exceeds README headline floor {:.2}x — \
         either the bench fixture grew, the CLI got chattier, or the README \
         claim needs revising. Investigate before relaxing the threshold.",
        overall_ratio,
        HEADLINE_MAX_RATIO,
    );
}

// ── Compile-time invariants (run on every `cargo test`) ─────────

#[test]
fn corpus_is_non_empty_and_well_formed() {
    // Deliberate compile-time-known assertion (see section header): the guard
    // exists to fail loudly if someone empties CORPUS, which clippy can prove
    // won't happen for the CURRENT const value — that proof is the point.
    #[allow(clippy::const_is_empty)]
    {
        assert!(!CORPUS.is_empty(), "CORPUS must define at least one task");
    }
    for task in CORPUS {
        assert!(!task.name.is_empty(), "task name must be non-empty");
        assert!(
            !task.args.is_empty(),
            "task '{}' must have at least one arg",
            task.name
        );
        assert!(
            task.baseline_bytes > 0,
            "task '{}' baseline_bytes must be > 0",
            task.name
        );
        assert!(
            task.max_ratio > 0.0 && task.max_ratio <= 1.0,
            "task '{}' max_ratio must be in (0, 1], got {}",
            task.name,
            task.max_ratio,
        );
    }
}

#[test]
fn corpus_task_names_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for task in CORPUS {
        assert!(
            seen.insert(task.name),
            "duplicate task name in CORPUS: {}",
            task.name,
        );
    }
}
