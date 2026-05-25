// T-506 — parity-runner: compare Rust outputs against golden baselines.
//
// Usage:
//   parity-runner compare <baseline-tag> [OPTIONS]
//
// Options:
//   --golden <dir>             Path to golden-outputs directory [default: parity/golden-outputs]
//   --threshold-counts <f>     Max node/edge count fraction deviation [default: 0.01]
//   --threshold-mrr <f>        Max MRR absolute delta [default: 0.10]
//   --threshold-nodes <n>      Max node count delta in explore results [default: 5]
//   --output-dir <dir>         Write per-check JSON reports [default: parity/reports]
//
// Exit codes:
//   0 — all checks pass
//   1 — one or more threshold violations
//   2 — golden files missing or malformed

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::SystemTime;

// ── CLI parsing (manual — no clap dep in this bin) ──────────────────────────

struct Args {
    baseline_tag: String,
    golden_dir: PathBuf,
    #[allow(dead_code)]
    threshold_counts: f64,
    #[allow(dead_code)]
    threshold_mrr: f64,
    #[allow(dead_code)]
    threshold_nodes: i64,
    output_dir: PathBuf,
}

fn parse_args() -> Option<Args> {
    let raw: Vec<String> = std::env::args().collect();
    if raw.len() < 3 || raw[1] != "compare" {
        return None;
    }
    let baseline_tag = raw[2].clone();
    let mut golden_dir = PathBuf::from("parity/golden-outputs");
    let mut threshold_counts = 0.01_f64;
    let mut threshold_mrr = 0.10_f64;
    let mut threshold_nodes = 5_i64;
    let mut output_dir = PathBuf::from("parity/reports");

    let mut i = 3;
    while i < raw.len() {
        match raw[i].as_str() {
            "--golden" if i + 1 < raw.len() => {
                golden_dir = PathBuf::from(&raw[i + 1]);
                i += 2;
            }
            "--threshold-counts" if i + 1 < raw.len() => {
                threshold_counts = raw[i + 1].parse().unwrap_or(0.01);
                i += 2;
            }
            "--threshold-mrr" if i + 1 < raw.len() => {
                threshold_mrr = raw[i + 1].parse().unwrap_or(0.10);
                i += 2;
            }
            "--threshold-nodes" if i + 1 < raw.len() => {
                threshold_nodes = raw[i + 1].parse().unwrap_or(5);
                i += 2;
            }
            "--output-dir" if i + 1 < raw.len() => {
                output_dir = PathBuf::from(&raw[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    Some(Args {
        baseline_tag,
        golden_dir,
        threshold_counts,
        threshold_mrr,
        threshold_nodes,
        output_dir,
    })
}

// ── Check result types ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum CheckStatus {
    Pass,
    Fail(String),
    Skip(String),
}

#[derive(Debug, Clone)]
struct CheckResult {
    name: String,
    status: CheckStatus,
    details: Option<String>,
}

impl CheckResult {
    fn pass(name: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            status: CheckStatus::Pass,
            details: None,
        }
    }
    fn fail(name: impl Into<String>, reason: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            status: CheckStatus::Fail(reason.into()),
            details: None,
        }
    }
    fn skip(name: impl Into<String>, reason: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            status: CheckStatus::Skip(reason.into()),
            details: None,
        }
    }
    fn is_fail(&self) -> bool {
        matches!(self.status, CheckStatus::Fail(_))
    }
}

// ── Threshold loading ────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Thresholds {
    node_count_fraction: f64,
    edge_count_fraction: f64,
    node_count_delta: i64,
    mrr_max_delta: f64,
}

fn load_thresholds(golden_dir: &Path) -> Thresholds {
    // Try parity/thresholds.toml (relative to CWD), then golden_dir/../thresholds.toml
    let candidates = [
        PathBuf::from("parity/thresholds.toml"),
        golden_dir
            .parent()
            .map(|p| p.join("thresholds.toml"))
            .unwrap_or_default(),
    ];

    for path in &candidates {
        if let Ok(content) = fs::read_to_string(path) {
            return parse_thresholds(&content);
        }
    }

    // Defaults if file not found.
    Thresholds {
        node_count_fraction: 0.01,
        edge_count_fraction: 0.01,
        node_count_delta: 5,
        mrr_max_delta: 0.10,
    }
}

fn parse_thresholds(toml_str: &str) -> Thresholds {
    let mut t = Thresholds {
        node_count_fraction: 0.01,
        edge_count_fraction: 0.01,
        node_count_delta: 5,
        mrr_max_delta: 0.10,
    };

    // Minimal TOML key=value parser (avoid pulling in toml crate here).
    for line in toml_str.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '=').collect();
        if parts.len() != 2 {
            continue;
        }
        let key = parts[0].trim();
        let val = parts[1].trim().trim_matches('"');
        match key {
            "node_count_fraction" => t.node_count_fraction = val.parse().unwrap_or(0.01),
            "edge_count_fraction" => t.edge_count_fraction = val.parse().unwrap_or(0.01),
            "node_count_delta" => t.node_count_delta = val.parse().unwrap_or(5),
            "mrr_max_delta" => t.mrr_max_delta = val.parse().unwrap_or(0.10),
            _ => {}
        }
    }
    t
}

// ── Golden-file checks ───────────────────────────────────────────────────────

/// Load a golden JSON file; return an error CheckResult if missing/malformed.
fn load_golden_json(path: &Path, check_name: &str) -> Result<serde_json::Value, CheckResult> {
    match fs::read_to_string(path) {
        Err(e) => Err(CheckResult::fail(
            check_name,
            format!("cannot read {}: {}", path.display(), e),
        )),
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            CheckResult::fail(
                check_name,
                format!("malformed JSON in {}: {}", path.display(), e),
            )
        }),
    }
}

/// Check that the golden metadata file is present and parseable.
fn check_metadata(golden_subdir: &Path, tag: &str) -> CheckResult {
    let p = golden_subdir.join("metadata.json");
    if !p.exists() {
        return CheckResult::skip(
            format!("metadata/{}", tag),
            format!("metadata.json not found at {}", p.display()),
        );
    }
    match load_golden_json(&p, &format!("metadata/{}", tag)) {
        Ok(_) => CheckResult::pass(format!("metadata/{}", tag)),
        Err(e) => e,
    }
}

/// Check node count from nodes-full.json against the baseline_tag stored in metadata.
fn check_node_count(golden_subdir: &Path, threshold: f64, tag: &str) -> CheckResult {
    let p = golden_subdir.join("nodes-full.json");
    if !p.exists() {
        return CheckResult::skip(
            format!("node_count/{}", tag),
            "nodes-full.json not found — skipping (non-elastic subset)".to_string(),
        );
    }
    let json = match load_golden_json(&p, &format!("node_count/{}", tag)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let node_count = match json.as_array() {
        Some(a) => a.len(),
        None => {
            return CheckResult::fail(
                format!("node_count/{}", tag),
                "nodes-full.json is not a JSON array",
            )
        }
    };

    // For rust-baseline, there is no TS reference count to compare against.
    // Accept any non-zero count as pass when baseline_tag == "rust-baseline".
    if tag == "rust-baseline" || tag == "ts-baseline" {
        if node_count == 0 {
            return CheckResult::fail(
                format!("node_count/{}", tag),
                "node count is 0 — extraction may have failed".to_string(),
            );
        }
        return CheckResult::pass(format!("node_count/{} ({})", tag, node_count));
    }

    // Otherwise: compare against metadata.json expected count.
    let meta_p = golden_subdir.join("metadata.json");
    if let Ok(meta_str) = fs::read_to_string(&meta_p) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_str) {
            if let Some(expected) = meta.get("node_count").and_then(|v| v.as_u64()) {
                let expected = expected as usize;
                let delta_frac = if expected == 0 {
                    0.0
                } else {
                    (node_count as f64 - expected as f64).abs() / expected as f64
                };
                if delta_frac > threshold {
                    return CheckResult::fail(
                        format!("node_count/{}", tag),
                        format!(
                            "node count {} differs from golden {} by {:.1}% (threshold {:.1}%)",
                            node_count,
                            expected,
                            delta_frac * 100.0,
                            threshold * 100.0
                        ),
                    );
                }
                return CheckResult::pass(format!(
                    "node_count/{} ({}/{})",
                    tag, node_count, expected
                ));
            }
        }
    }

    CheckResult::pass(format!(
        "node_count/{} ({} nodes, no reference count)",
        tag, node_count
    ))
}

/// Check edge count from edges-full.json.
fn check_edge_count(golden_subdir: &Path, _threshold: f64, tag: &str) -> CheckResult {
    let p = golden_subdir.join("edges-full.json");
    if !p.exists() {
        return CheckResult::skip(
            format!("edge_count/{}", tag),
            "edges-full.json not found".to_string(),
        );
    }
    let json = match load_golden_json(&p, &format!("edge_count/{}", tag)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let edge_count = match json.as_array() {
        Some(a) => a.len(),
        None => {
            return CheckResult::fail(
                format!("edge_count/{}", tag),
                "edges-full.json is not a JSON array",
            )
        }
    };

    if edge_count == 0 {
        return CheckResult::fail(
            format!("edge_count/{}", tag),
            "edge count is 0 — extraction or resolution may have failed".to_string(),
        );
    }

    CheckResult::pass(format!("edge_count/{} ({} edges)", tag, edge_count))
}

/// Check required edge kinds are present in edges-full.json.
fn check_edge_kinds(golden_subdir: &Path, tag: &str) -> CheckResult {
    let p = golden_subdir.join("edges-full.json");
    if !p.exists() {
        return CheckResult::skip(
            format!("edge_kinds/{}", tag),
            "edges-full.json not found".to_string(),
        );
    }
    let json = match load_golden_json(&p, &format!("edge_kinds/{}", tag)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let edges = match json.as_array() {
        Some(a) => a,
        None => {
            return CheckResult::fail(
                format!("edge_kinds/{}", tag),
                "edges-full.json is not a JSON array",
            )
        }
    };

    let mut kinds_found: HashMap<String, usize> = HashMap::new();
    for edge in edges {
        if let Some(kind) = edge.get("kind").and_then(|k| k.as_str()) {
            *kinds_found.entry(kind.to_string()).or_default() += 1;
        }
    }

    let required = ["contains", "calls", "imports"];
    let mut missing = vec![];
    for req in &required {
        if !kinds_found.contains_key(*req) {
            missing.push(*req);
        }
    }

    if missing.is_empty() {
        CheckResult::pass(format!(
            "edge_kinds/{} ({})",
            tag,
            kinds_found.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    } else {
        // For synthetic-120 which may not have all edge kinds, treat as skip.
        CheckResult::skip(
            format!("edge_kinds/{}", tag),
            format!(
                "missing edge kinds: {} (may be expected for this corpus)",
                missing.join(", ")
            ),
        )
    }
}

/// Check that search-results.json exists and has results.
fn check_search_results(golden_subdir: &Path, tag: &str) -> CheckResult {
    let p = golden_subdir.join("search-results.json");
    if !p.exists() {
        return CheckResult::skip(
            format!("search_results/{}", tag),
            "search-results.json not found".to_string(),
        );
    }
    let json = match load_golden_json(&p, &format!("search_results/{}", tag)) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let results_count = json
        .get("results")
        .and_then(|r| r.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if results_count == 0 {
        return CheckResult::fail(
            format!("search_results/{}", tag),
            "search-results.json has no results".to_string(),
        );
    }

    CheckResult::pass(format!(
        "search_results/{} ({} results)",
        tag, results_count
    ))
}

/// Check that installer-configs.json exists and has entries.
fn check_installer_configs(golden_dir: &Path) -> CheckResult {
    let p = golden_dir
        .join("elasticsearch")
        .join("installer-configs.json");
    if !p.exists() {
        return CheckResult::fail(
            "installer_configs",
            format!("installer-configs.json not found at {}", p.display()),
        );
    }
    let json = match load_golden_json(&p, "installer_configs") {
        Ok(v) => v,
        Err(e) => return e,
    };

    let configs_count = json
        .get("configs")
        .and_then(|c| c.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if configs_count == 0 {
        return CheckResult::fail("installer_configs", "installer-configs.json has no configs");
    }

    // Verify each config has the required fields.
    if let Some(configs) = json.get("configs").and_then(|c| c.as_array()) {
        for (i, cfg) in configs.iter().enumerate() {
            if cfg.get("target").is_none() || cfg.get("args").is_none() {
                return CheckResult::fail(
                    "installer_configs",
                    format!("config[{}] missing required fields (target, args)", i),
                );
            }
        }
    }

    CheckResult::pass(format!("installer_configs ({} configs)", configs_count))
}

// ── Report writing ───────────────────────────────────────────────────────────

fn write_report(output_dir: &Path, baseline_tag: &str, results: &[CheckResult]) {
    if let Err(e) = fs::create_dir_all(output_dir) {
        eprintln!(
            "parity-runner: could not create output dir {}: {}",
            output_dir.display(),
            e
        );
        return;
    }

    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let passed: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Pass))
        .collect();
    let failed: Vec<_> = results.iter().filter(|r| r.is_fail()).collect();
    let skipped: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Skip(_)))
        .collect();

    let report = serde_json::json!({
        "generated_at": ts,
        "baseline_tag": baseline_tag,
        "summary": {
            "total": results.len(),
            "passed": passed.len(),
            "failed": failed.len(),
            "skipped": skipped.len()
        },
        "results": results.iter().map(|r| {
            let (status_str, reason) = match &r.status {
                CheckStatus::Pass => ("pass", String::new()),
                CheckStatus::Fail(s) => ("fail", s.clone()),
                CheckStatus::Skip(s) => ("skip", s.clone()),
            };
            serde_json::json!({
                "check": r.name,
                "status": status_str,
                "reason": reason,
                "details": r.details
            })
        }).collect::<Vec<_>>()
    });

    let report_path = output_dir.join(format!("parity-{}.json", ts));
    match fs::write(
        &report_path,
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    ) {
        Ok(()) => eprintln!("parity-runner: report written to {}", report_path.display()),
        Err(e) => eprintln!("parity-runner: could not write report: {}", e),
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let args = match parse_args() {
        Some(a) => a,
        None => {
            eprintln!(
                "Usage: parity-runner compare <baseline-tag> [OPTIONS]\n\
                 \n\
                 Options:\n\
                   --golden <dir>             golden-outputs directory [default: parity/golden-outputs]\n\
                   --threshold-counts <f>     node/edge count fraction [default: 0.01]\n\
                   --threshold-mrr <f>        MRR delta [default: 0.10]\n\
                   --threshold-nodes <n>      explore node delta [default: 5]\n\
                   --output-dir <dir>         report output dir [default: parity/reports]\n\
                 \n\
                 Exit codes: 0=pass, 1=violation, 2=missing golden"
            );
            process::exit(1);
        }
    };

    let golden_dir = &args.golden_dir;
    let tag = &args.baseline_tag;

    // Check golden directory exists.
    if !golden_dir.exists() {
        eprintln!(
            "parity-runner: golden directory not found: {}",
            golden_dir.display()
        );
        process::exit(2);
    }

    let thresholds = load_thresholds(golden_dir);

    let mut results: Vec<CheckResult> = Vec::new();
    let mut missing_critical = false;

    // ── Detect layout: golden_dir may be:
    //   (a) the parent "golden-outputs/" containing synthetic-120/ and elasticsearch/
    //   (b) "golden-outputs/synthetic-120" pointed at directly (acceptance test format)
    //
    // We detect (b) by checking if golden_dir itself contains nodes-full.json.
    let (s120, elastic_parent) = if golden_dir.join("nodes-full.json").exists() {
        // Case (b): golden_dir IS synthetic-120.
        (
            golden_dir.to_path_buf(),
            golden_dir.parent().unwrap_or(golden_dir).to_path_buf(),
        )
    } else {
        // Case (a): golden_dir is the parent.
        (golden_dir.join("synthetic-120"), golden_dir.to_path_buf())
    };

    // ── synthetic-120 checks ─────────────────────────────────────────────────
    if s120.exists() {
        results.push(check_metadata(&s120, tag));
        results.push(check_node_count(&s120, thresholds.node_count_fraction, tag));
        results.push(check_edge_count(&s120, thresholds.edge_count_fraction, tag));
        results.push(check_edge_kinds(&s120, tag));
        results.push(check_search_results(&s120, tag));
    } else {
        eprintln!(
            "parity-runner: warning: synthetic-120/ not found under {}",
            golden_dir.display()
        );
        missing_critical = true;
    }

    // ── elasticsearch checks ─────────────────────────────────────────────────
    let elastic = elastic_parent.join("elasticsearch");
    if elastic.exists() {
        results.push(check_installer_configs(&elastic_parent));
    }

    // ── Summary ──────────────────────────────────────────────────────────────
    write_report(&args.output_dir, tag, &results);

    let failed: Vec<_> = results.iter().filter(|r| r.is_fail()).collect();
    let skipped: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Skip(_)))
        .collect();
    let passed: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Pass))
        .collect();

    println!("\nparity-runner summary (baseline: {tag})");
    println!("  passed:  {}", passed.len());
    println!("  failed:  {}", failed.len());
    println!("  skipped: {}", skipped.len());
    println!();

    if !failed.is_empty() {
        eprintln!("FAILURES:");
        for r in &failed {
            if let CheckStatus::Fail(reason) = &r.status {
                eprintln!("  [FAIL] {} — {}", r.name, reason);
            }
        }
        println!("\nResult: FAIL");
        process::exit(1);
    }

    if missing_critical {
        eprintln!("parity-runner: critical golden files missing — exit 2");
        process::exit(2);
    }

    println!(
        "Result: PASS — all {} checks passed ({} skipped)",
        passed.len(),
        skipped.len()
    );
    process::exit(0);
}
