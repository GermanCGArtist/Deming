//! Test completion rule evaluation (`ExitCodeZero`, `LogPattern`, `GameTestReport`, `Composite`).

use std::fs;
use std::path::{Path, PathBuf};

use deming_core::{DemingError, Result, TestCompletionRule};
use regex::Regex;
use serde_json::Value;

/// Inputs collected from an executed client instance for rule evaluation.
#[derive(Debug, Clone, Default)]
pub struct EvaluationContext {
    /// Process exit code, if the process exited normally or with a status.
    pub exit_code: Option<i32>,
    /// Whether the instance was terminated due to timeout.
    pub timed_out: bool,
    /// Whether the instance was terminated due to an explicit abort.
    pub aborted: bool,
    /// Captured log lines (stdout/stderr/latest.log) in chronological order.
    pub logs: Vec<String>,
    /// Root working directory of the instance (base for relative `GameTestReport` globs).
    pub instance_dir: PathBuf,
}

/// Result of evaluating a [`TestCompletionRule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationResult {
    /// Whether the rule succeeded.
    pub passed: bool,
    /// Human-readable summary explaining why the rule passed or failed.
    pub reason: String,
}

/// Evaluates `rule` against `ctx`.
pub fn evaluate_rule(
    rule: &TestCompletionRule,
    ctx: &EvaluationContext,
) -> Result<EvaluationResult> {
    if ctx.aborted {
        return Ok(EvaluationResult {
            passed: false,
            reason: "instance was aborted".to_string(),
        });
    }
    if ctx.timed_out {
        return Ok(EvaluationResult {
            passed: false,
            reason: "instance timed out".to_string(),
        });
    }

    evaluate_rule_inner(rule, ctx)
}

fn evaluate_rule_inner(
    rule: &TestCompletionRule,
    ctx: &EvaluationContext,
) -> Result<EvaluationResult> {
    match rule {
        TestCompletionRule::ExitCodeZero => match ctx.exit_code {
            Some(0) => Ok(EvaluationResult {
                passed: true,
                reason: "process exited with code 0".to_string(),
            }),
            Some(code) => Ok(EvaluationResult {
                passed: false,
                reason: format!("process exited with non-zero code {code}"),
            }),
            None => Ok(EvaluationResult {
                passed: false,
                reason: "process exited without an exit code".to_string(),
            }),
        },

        TestCompletionRule::LogPattern {
            success_regex,
            failure_regex,
        } => {
            let success_re = Regex::new(success_regex).map_err(|e| {
                DemingError::InvalidSpec(format!("invalid success_regex '{success_regex}': {e}"))
            })?;

            if let Some(fail_pat) = failure_regex {
                let fail_re = Regex::new(fail_pat).map_err(|e| {
                    DemingError::InvalidSpec(format!("invalid failure_regex '{fail_pat}': {e}"))
                })?;
                if let Some(matched) = ctx.logs.iter().find(|line| fail_re.is_match(line)) {
                    return Ok(EvaluationResult {
                        passed: false,
                        reason: format!("failure_regex '{fail_pat}' matched log line: {matched}"),
                    });
                }
            }

            if ctx.logs.iter().any(|line| success_re.is_match(line)) {
                Ok(EvaluationResult {
                    passed: true,
                    reason: format!("success_regex '{success_regex}' matched"),
                })
            } else {
                Ok(EvaluationResult {
                    passed: false,
                    reason: format!("success_regex '{success_regex}' did not match any log line"),
                })
            }
        }

        TestCompletionRule::GameTestReport { report_path_glob } => {
            evaluate_gametest_reports(&ctx.instance_dir, report_path_glob)
        }

        TestCompletionRule::Composite(rules) => {
            if rules.is_empty() {
                return Err(DemingError::InvalidSpec(
                    "Composite TestCompletionRule must contain at least one sub-rule".to_string(),
                ));
            }

            let mut pass_reasons = Vec::new();
            let mut fail_reasons = Vec::new();

            for sub in rules {
                let res = evaluate_rule_inner(sub, ctx)?;
                if res.passed {
                    pass_reasons.push(res.reason);
                } else {
                    fail_reasons.push(res.reason);
                }
            }

            if fail_reasons.is_empty() {
                Ok(EvaluationResult {
                    passed: true,
                    reason: pass_reasons.join("; "),
                })
            } else {
                Ok(EvaluationResult {
                    passed: false,
                    reason: fail_reasons.join("; "),
                })
            }
        }
    }
}

fn evaluate_gametest_reports(instance_dir: &Path, report_path_glob: &str) -> Result<EvaluationResult> {
    let matched_files = find_matching_files(instance_dir, report_path_glob)?;
    if matched_files.is_empty() {
        return Ok(EvaluationResult {
            passed: false,
            reason: format!("no GameTest report files matched '{report_path_glob}'"),
        });
    }

    let mut total_reports = 0usize;
    let mut failures = Vec::new();

    for path in &matched_files {
        total_reports += 1;
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let content = fs::read_to_string(path)?;
        let verdict = match ext.as_str() {
            "xml" => parse_junit_xml_report(path, &content),
            "json" => parse_json_report(path, &content),
            other => Err(format!(
                "unsupported GameTest report extension '.{other}' in {}",
                path.display()
            )),
        };

        if let Err(msg) = verdict {
            failures.push(msg);
        }
    }

    if failures.is_empty() {
        Ok(EvaluationResult {
            passed: true,
            reason: format!("all {total_reports} GameTest report(s) passed"),
        })
    } else {
        Ok(EvaluationResult {
            passed: false,
            reason: failures.join("; "),
        })
    }
}

fn parse_junit_xml_report(path: &Path, content: &str) -> std::result::Result<(), String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("report.xml");

    let tag_re = Regex::new(r"<testsuite\b([^>]*)>").unwrap();
    let attr_tests = Regex::new(r#"\btests\s*=\s*["'](\d+)["']"#).unwrap();
    let attr_failures = Regex::new(r#"\bfailures\s*=\s*["'](\d+)["']"#).unwrap();
    let attr_errors = Regex::new(r#"\berrors\s*=\s*["'](\d+)["']"#).unwrap();

    let mut suite_count = 0usize;
    let mut total_tests = 0u64;
    let mut total_failures = 0u64;
    let mut total_errors = 0u64;

    for cap in tag_re.captures_iter(content) {
        suite_count += 1;
        let attrs = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if let Some(c) = attr_tests.captures(attrs) {
            total_tests += c[1].parse::<u64>().unwrap_or(0);
        }
        if let Some(c) = attr_failures.captures(attrs) {
            total_failures += c[1].parse::<u64>().unwrap_or(0);
        }
        if let Some(c) = attr_errors.captures(attrs) {
            total_errors += c[1].parse::<u64>().unwrap_or(0);
        }
    }

    if suite_count == 0 {
        return Err(format!("{name}: no <testsuite> element found"));
    }

    let has_failure_tag = content.contains("<failure") || content.contains("<error");
    if total_failures > 0 || total_errors > 0 || has_failure_tag {
        return Err(format!(
            "{name}: GameTest suite reported failures={total_failures}, errors={total_errors}"
        ));
    }

    if total_tests == 0 {
        return Err(format!("{name}: GameTest suite reported 0 tests"));
    }

    Ok(())
}

fn parse_json_report(path: &Path, content: &str) -> std::result::Result<(), String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("report.json");

    let value: Value =
        serde_json::from_str(content).map_err(|e| format!("{name}: invalid JSON: {e}"))?;

    match &value {
        Value::Object(map) => {
            if let Some(passed) = map.get("passed").and_then(Value::as_bool) {
                return if passed {
                    Ok(())
                } else {
                    Err(format!("{name}: report has passed=false"))
                };
            }
            if let Some(success) = map.get("success").and_then(Value::as_bool) {
                return if success {
                    Ok(())
                } else {
                    Err(format!("{name}: report has success=false"))
                };
            }
            let failures = map.get("failures").and_then(Value::as_u64).unwrap_or(0)
                + map.get("failed").and_then(Value::as_u64).unwrap_or(0)
                + map.get("errors").and_then(Value::as_u64).unwrap_or(0);
            if failures > 0 {
                return Err(format!("{name}: report has {failures} failure(s)/error(s)"));
            }
            if let Some(results) = map.get("results").and_then(Value::as_array) {
                return validate_json_result_array(name, results);
            }
            if map.contains_key("tests") {
                let tests = map.get("tests").and_then(Value::as_u64).unwrap_or(0);
                return if tests > 0 {
                    Ok(())
                } else {
                    Err(format!("{name}: report has 0 tests"))
                };
            }
            Err(format!("{name}: unrecognized JSON GameTest report schema"))
        }
        Value::Array(items) => validate_json_result_array(name, items),
        _ => Err(format!("{name}: expected JSON object or array")),
    }
}

fn validate_json_result_array(name: &str, items: &[Value]) -> std::result::Result<(), String> {
    if items.is_empty() {
        return Err(format!("{name}: report contains 0 test results"));
    }
    for (idx, item) in items.iter().enumerate() {
        if let Some(passed) = item.get("passed").and_then(Value::as_bool) {
            if !passed {
                let test_name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Err(format!("{name}: test '{test_name}' (#{idx}) failed"));
            }
        } else if let Some(status) = item.get("status").and_then(Value::as_str) {
            let ok = matches!(
                status.to_ascii_lowercase().as_str(),
                "passed" | "pass" | "ok" | "success"
            );
            if !ok {
                let test_name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Err(format!(
                    "{name}: test '{test_name}' (#{idx}) had status '{status}'"
                ));
            }
        } else {
            return Err(format!(
                "{name}: test entry #{idx} missing 'passed' or 'status' field"
            ));
        }
    }
    Ok(())
}

/// Finds files matching `glob_pattern` relative to `base_dir` (or absolute if `glob_pattern` is rooted).
pub(crate) fn find_matching_files(base_dir: &Path, glob_pattern: &str) -> Result<Vec<PathBuf>> {
    let normalized = glob_pattern.replace('\\', "/");
    let glob_path = Path::new(&normalized);

    let (search_root, rel_pattern) = if glob_path.is_absolute() {
        // Split into longest non-wildcard prefix and remaining pattern.
        let mut root = PathBuf::new();
        let mut rest_parts = Vec::new();
        let mut hit_wildcard = false;
        for comp in glob_path.components() {
            let s = comp.as_os_str().to_string_lossy();
            if !hit_wildcard && !s.contains('*') && !s.contains('?') {
                root.push(comp.as_os_str());
            } else {
                hit_wildcard = true;
                rest_parts.push(s.into_owned());
            }
        }
        (root, rest_parts.join("/"))
    } else {
        (base_dir.to_path_buf(), normalized)
    };

    if !search_root.exists() {
        return Ok(Vec::new());
    }

    let mut all_files = Vec::new();
    collect_files_recursive(&search_root, &mut all_files)?;

    let pattern_regex = glob_to_regex(&rel_pattern)?;
    let mut matched = Vec::new();
    for file in all_files {
        if let Ok(rel) = file.strip_prefix(&search_root) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if pattern_regex.is_match(&rel_str) {
                matched.push(file);
            }
        }
    }
    matched.sort();
    Ok(matched)
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn glob_to_regex(glob: &str) -> Result<Regex> {
    let chars: Vec<char> = glob.chars().collect();
    let mut re = String::from("^");
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        re.push_str("(?:.*/)?");
                        i += 3;
                        continue;
                    } else {
                        re.push_str(".*");
                        i += 2;
                        continue;
                    }
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\' => {
                re.push('\\');
                re.push(chars[i]);
            }
            c => re.push(c),
        }
        i += 1;
    }
    re.push('$');
    Regex::new(&re)
        .map_err(|e| DemingError::InvalidSpec(format!("invalid glob pattern '{glob}': {e}")))
}
