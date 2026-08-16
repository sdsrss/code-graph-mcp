use super::*;

/// CLI arguments for the `tour` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp tour",
    about = "Dependency-ordered reading order: where to start reading a repo (or subtree)"
)]
pub struct TourArgs {
    /// Optional path prefix to scope the tour to a subtree (omit = whole project;
    /// absolute paths under the project root are accepted)
    pub path: Option<String>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// True when module directory `module_path` is the prefix `pre` or sits under it.
/// `pre` is a normalized path; an empty prefix (from "." or omitted) matches all.
pub(crate) fn module_under_prefix(module_path: &str, pre: &str) -> bool {
    let pre = pre.trim_end_matches('/');
    pre.is_empty() || module_path == pre || module_path.starts_with(&format!("{}/", pre))
}

/// Reading order — lists a module's prerequisites before the modules that build
/// on them (Kahn topological sort over import edges), so reading top-to-bottom
/// orients you from the ground up. Reuses the project-map graph; read-only.
pub fn cmd_tour(project_root: &Path, args: TourArgs) -> Result<()> {
    use crate::graph::reading_order::compute_reading_order;

    let json_mode = args.json;

    // Optional subtree scope. Omitted → whole project.
    let scope: Option<String> = match args.path.as_deref() {
        None => None,
        Some("") => anyhow::bail!("path must not be empty — omit it to tour the whole project"),
        Some(raw) => Some(normalize_user_path(project_root, raw)?),
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (modules, deps, entry_points, _hot) = queries::get_project_map(conn)?;

    let modules: Vec<_> = match &scope {
        None => modules,
        Some(prefix) => modules
            .into_iter()
            .filter(|m| module_under_prefix(&m.path, prefix))
            .collect(),
    };

    let order = compute_reading_order(&modules, &deps, &entry_points);

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Object envelope (cli_json_empty contract: same shape on the empty path).
        let arr: Vec<serde_json::Value> = order
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path,
                    "role": e.role.as_str(),
                    "depended_on_by": e.depended_on_by,
                    "depends_on": e.depends_on,
                    "key_symbols": e.key_symbols,
                    "in_cycle": e.in_cycle,
                })
            })
            .collect();
        let result = serde_json::json!({ "reading_order": arr });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    if order.is_empty() {
        match &scope {
            Some(p) => writeln!(stdout, "(no indexed modules under: {})", p)?,
            None => writeln!(stdout, "(empty project — no indexed source files)")?,
        }
        return Ok(());
    }

    let cycles = order.iter().filter(|e| e.in_cycle).count();
    if cycles > 0 {
        writeln!(
            stdout,
            "Reading order (foundational → entry; {} modules, {} via cycle-break):",
            order.len(),
            cycles
        )?;
    } else {
        writeln!(
            stdout,
            "Reading order (foundational → entry; {} modules):",
            order.len()
        )?;
    }
    for (i, e) in order.iter().enumerate() {
        let mut annot: Vec<String> = vec![format!("[{}]", e.role.as_str())];
        if e.in_cycle {
            annot.push("[cycle]".to_string());
        }
        if e.depended_on_by > 0 {
            annot.push(format!("depended-on-by {}", e.depended_on_by));
        }
        if !e.depends_on.is_empty() {
            let shown = e
                .depends_on
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(",");
            let extra = e.depends_on.len().saturating_sub(3);
            let suffix = if extra > 0 {
                format!("+{}", extra)
            } else {
                String::new()
            };
            annot.push(format!("imports {}{}", shown, suffix));
        }
        write!(stdout, "  {:>2}. {}  {}", i + 1, e.path, annot.join(" · "))?;
        if !e.key_symbols.is_empty() {
            let syms = e
                .key_symbols
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            write!(stdout, "  — {}", syms)?;
        }
        writeln!(stdout)?;
    }

    Ok(())
}

// --- overview subcommand ---
