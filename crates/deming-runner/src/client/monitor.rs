use std::sync::{Arc, Mutex};

use chrono::Utc;
use deming_core::{InstanceState, RunEvent, TestCompletionRule};
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Returns `Some((success_regexes, failure_regexes))` if `rule` depends purely on log patterns
/// (i.e. a [`TestCompletionRule::LogPattern`] or a [`TestCompletionRule::Composite`] without
/// `ExitCodeZero` or `GameTestReport`), allowing [`super::run_client`] to terminate the client
/// cleanly as soon as the rule passes or fails without waiting for `timeout_secs`.
pub fn rule_supports_early_completion(
    rule: &TestCompletionRule,
) -> Option<(Vec<Regex>, Vec<Regex>)> {
    match rule {
        TestCompletionRule::LogPattern {
            success_regex,
            failure_regex,
        } => {
            let succ = Regex::new(success_regex).ok()?;
            let mut fails = Vec::new();
            if let Some(pat) = failure_regex {
                fails.push(Regex::new(pat).ok()?);
            }
            Some((vec![succ], fails))
        }
        TestCompletionRule::Composite(sub_rules) => {
            if sub_rules.is_empty() {
                return None;
            }
            let mut all_succ = Vec::new();
            let mut all_fail = Vec::new();
            for sub in sub_rules {
                let (s, f) = rule_supports_early_completion(sub)?;
                all_succ.extend(s);
                all_fail.extend(f);
            }
            Some((all_succ, all_fail))
        }
        TestCompletionRule::ExitCodeZero | TestCompletionRule::GameTestReport { .. } => None,
    }
}

pub(super) struct EarlyMatchTracker {
    success_res: Vec<Regex>,
    failure_res: Vec<Regex>,
    matched_success: Vec<bool>,
    tx: Option<oneshot::Sender<()>>,
}

impl EarlyMatchTracker {
    pub(super) fn new(
        success_res: Vec<Regex>,
        failure_res: Vec<Regex>,
        tx: oneshot::Sender<()>,
    ) -> Self {
        let len = success_res.len();
        Self {
            success_res,
            failure_res,
            matched_success: vec![false; len],
            tx: Some(tx),
        }
    }

    pub(super) fn check_line(&mut self, line: &str) {
        if self.tx.is_none() {
            return;
        }
        if self.failure_res.iter().any(|re| re.is_match(line)) {
            if let Some(tx) = self.tx.take() {
                let _ = tx.send(());
            }
            return;
        }
        for (idx, re) in self.success_res.iter().enumerate() {
            if !self.matched_success[idx] && re.is_match(line) {
                self.matched_success[idx] = true;
            }
        }
        if !self.matched_success.is_empty() && self.matched_success.iter().all(|&m| m) {
            if let Some(tx) = self.tx.take() {
                let _ = tx.send(());
            }
        }
    }
}

pub(super) fn spawn_log_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stream: R,
    stream_buf: Arc<Mutex<Vec<String>>>,
    combined_buf: Arc<Mutex<Vec<String>>>,
    events: Option<mpsc::UnboundedSender<RunEvent>>,
    run_id: Uuid,
    source: String,
    early_tracker: Option<Arc<Mutex<EarlyMatchTracker>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut g) = stream_buf.lock() {
                g.push(line.clone());
            }
            if let Ok(mut g) = combined_buf.lock() {
                g.push(line.clone());
            }
            if let Some(tx) = &events {
                let _ = tx.send(RunEvent::LogLine {
                    run_id,
                    source: source.clone(),
                    line: line.clone(),
                    timestamp: Utc::now(),
                });
            }
            if let Some(tracker) = &early_tracker {
                if let Ok(mut t) = tracker.lock() {
                    t.check_line(&line);
                }
            }
        }
    })
}

pub(super) fn emit_state(
    events: &Option<mpsc::UnboundedSender<RunEvent>>,
    run_id: Uuid,
    instance_id: &str,
    state: InstanceState,
    message: Option<String>,
) {
    if let Some(tx) = events {
        let _ = tx.send(RunEvent::InstanceStateChanged {
            run_id,
            instance_id: instance_id.to_string(),
            state,
            message,
            timestamp: Utc::now(),
        });
    }
}
