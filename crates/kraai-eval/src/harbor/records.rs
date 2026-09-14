use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use serde_json::Value;

use super::JsonField;
use super::metrics::{duration, trial_metrics};
use super::{HarborJobIdentity, HarborModelSettings, HarborTrialStatus, HarborTrialSummary};

pub(super) struct HarborJob {
    pub identity: HarborJobIdentity,
    pub settings: Value,
    pub trials: BTreeMap<(String, String), Vec<Trial>>,
}

pub(super) struct Trial {
    pub summary: HarborTrialSummary,
    pub settings: Value,
    started_at: chrono::DateTime<chrono::FixedOffset>,
}

pub(super) fn load_job(directory: &Path) -> Result<HarborJob> {
    let directory = directory
        .canonicalize()
        .wrap_err("resolve Harbor job directory")?;
    let lock = read_json(&directory.join("lock.json"))?;
    let result = read_json(&directory.join("result.json"))?;
    ensure!(
        lock.field("/schema_version") == 3,
        "unsupported Harbor job lock schema"
    );
    ensure!(
        lock.field("/harbor/version") == "0.22.0",
        "Harbor job must record pinned version 0.22.0"
    );
    ensure!(
        !result.field("/finished_at").is_null(),
        "Harbor job is unfinished"
    );
    duration(result.field("/started_at"), result.field("/finished_at"))?;
    ensure!(
        result.field("/stats/n_retries") == 0,
        "Harbor retries cannot be compared without complete retry usage"
    );
    let job_id = required_string(result.field("/id"), "Harbor job ID")?;
    let locks = lock
        .field("/trials")
        .as_array()
        .ok_or_else(|| eyre!("Harbor job lock has no trial population"))?;
    ensure!(!locks.is_empty(), "Harbor job has no planned trials");
    ensure!(
        result.field("/n_total_trials").as_u64() == Some(locks.len() as u64),
        "Harbor planned trial counts disagree"
    );
    ensure!(
        result.field("/stats/n_completed_trials").as_u64() == Some(locks.len() as u64),
        "Harbor job has incomplete trials"
    );
    let mut pending = BTreeMap::<String, u64>::new();
    for trial in locks {
        validate_lock(trial)?;
        *pending.entry(json_hash(trial)?).or_default() += 1;
    }
    let mut trials = BTreeMap::<(String, String), Vec<Trial>>::new();
    let mut identities = BTreeSet::new();
    let mut harness_names = BTreeSet::new();
    let mut trial_names = BTreeSet::new();
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        if !path.is_dir() || !path.join("result.json").is_file() {
            continue;
        }
        let path = path.canonicalize()?;
        ensure!(
            path.starts_with(&directory),
            "Harbor trial directory escapes its job"
        );
        let trial_lock = read_json(&path.join("lock.json"))?;
        let digest = json_hash(&trial_lock)?;
        let count = pending.get_mut(&digest).ok_or_else(|| {
            eyre!(
                "Harbor trial {} was not in the planned population",
                path.display()
            )
        })?;
        ensure!(*count > 0, "Harbor job contains duplicate trial results");
        *count -= 1;
        let trial_result = read_json(&path.join("result.json"))?;
        ensure!(
            trial_result.field("/config/job_id").as_str() == Some(job_id.as_str()),
            "Harbor trial job ID disagrees"
        );
        let name = required_string(trial_result.field("/trial_name"), "Harbor trial name")?;
        ensure!(
            path.file_name().is_some_and(|file| file == name.as_str()),
            "Harbor trial directory and name disagree"
        );
        ensure!(
            trial_names.insert(name.clone()),
            "duplicate Harbor trial name"
        );
        let task_name = required_string(trial_lock.field("/task/name"), "Harbor task name")?;
        ensure!(
            trial_result.field("/task_name").as_str() == Some(task_name.as_str()),
            "Harbor trial task name disagrees with its lock"
        );
        ensure!(
            trial_result
                .field("/step_results")
                .as_array()
                .is_none_or(Vec::is_empty),
            "multi-step Harbor trials are not supported by this comparison"
        );
        let configured_model = required_string(
            trial_lock.field("/agent/model_name"),
            "Harbor configured model",
        )?;
        ensure!(
            trial_result.field("/config/agent/model_name").as_str()
                == Some(configured_model.as_str()),
            "Harbor result configured model disagrees with its lock"
        );
        let agent_name =
            required_string(trial_result.field("/agent_info/name"), "Harbor agent name")?;
        let agent_version = required_string(
            trial_result.field("/agent_info/version"),
            "Harbor agent version",
        )?;
        identities.insert((agent_name, agent_version.clone(), configured_model.clone()));
        let exception_type = trial_result
            .field("/exception_info/exception_type")
            .as_str()
            .map(str::to_owned);
        let reward_value = trial_result.field("/verifier_result/rewards/reward");
        let reward = if reward_value.is_null() {
            None
        } else {
            Some(
                reward_value
                    .as_f64()
                    .ok_or_else(|| eyre!("invalid Harbor reward"))?,
            )
        };
        ensure!(
            reward.is_none_or(|reward| reward == 0.0 || reward == 1.0),
            "Harbor comparison requires a binary reward field"
        );
        let status = match (exception_type.as_deref(), reward) {
            (_, Some(1.0)) => HarborTrialStatus::Passed,
            (_, Some(0.0)) => HarborTrialStatus::Failed,
            (
                Some("AgentTimeoutError" | "NonZeroAgentExitCodeError" | "AgentSafetyRefusalError"),
                _,
            ) => HarborTrialStatus::AgentFailed,
            (Some(_), _) | (None, None) => HarborTrialStatus::InfrastructureError,
            _ => HarborTrialStatus::Failed,
        };
        let adapter = profile_adapter(&trial_lock);
        let require_controller = adapter && status != HarborTrialStatus::InfrastructureError;
        let runner_metrics = path.join("kraai-controller/runner-metrics.json");
        ensure!(
            !require_controller || runner_metrics.is_file(),
            "Harbor profile adapter is missing private controller runner metadata"
        );
        if runner_metrics.is_file() {
            let runner = read_json(&runner_metrics)?;
            ensure!(
                runner.field("/schema_version") == 1
                    && runner.field("/model").as_str() == Some(configured_model.as_str()),
                "Harbor runner metadata disagrees with configured model"
            );
            ensure!(
                runner.field("/bundle_sha256").as_str() == Some(agent_version.as_str()),
                "Harbor runner bundle differs from recorded agent version"
            );
            harness_names.insert(required_string(
                runner.field("/harness"),
                "Harbor harness name",
            )?);
        }
        let (proxy, proxy_identity_sha256, model_settings) =
            proxy_metadata(&path, &trial_lock, require_controller)?;
        let started_at = chrono::DateTime::parse_from_rfc3339(&required_string(
            trial_result.field("/started_at"),
            "Harbor trial start timestamp",
        )?)?;
        let summary = HarborTrialSummary {
            directory: path.clone(),
            trial_name: name,
            task_source: trial_lock.field("/task/source").as_str().map(str::to_owned),
            status,
            reward,
            exception_type,
            model_settings,
            proxy_identity_sha256,
            lock_sha256: digest,
            result_sha256: json_hash(&trial_result)?,
            metrics: trial_metrics(&trial_result, proxy, !adapter)?,
        };
        trials
            .entry((
                task_name,
                required_string(trial_lock.field("/task/digest"), "Harbor task digest")?,
            ))
            .or_default()
            .push(Trial {
                summary,
                settings: trial_settings(&trial_lock)?,
                started_at,
            });
    }
    ensure!(
        pending.values().all(|count| *count == 0),
        "Harbor job is missing planned trial results"
    );
    ensure!(
        identities.len() == 1 && harness_names.len() <= 1,
        "Harbor job must contain exactly one harness version and model"
    );
    let (agent, agent_version, configured_model) = identities
        .into_iter()
        .next()
        .ok_or_else(|| eyre!("Harbor job has no trial results"))?;
    for task_trials in trials.values_mut() {
        task_trials.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.summary.trial_name.cmp(&right.summary.trial_name))
        });
    }
    Ok(HarborJob {
        identity: HarborJobIdentity {
            directory,
            job_id,
            harbor_version: String::from("0.22.0"),
            harness: harness_names.into_iter().next().unwrap_or(agent),
            agent_version,
            configured_model,
            lock_sha256: json_hash(&lock)?,
            result_sha256: json_hash(&result)?,
        },
        settings: serde_json::json!({"harbor": lock.field("/harbor"), "concurrency": lock.field("/n_concurrent_trials"), "retry": lock.field("/retry")}),
        trials,
    })
}

fn validate_lock(lock: &Value) -> Result<()> {
    ensure!(
        lock.field("/schema_version") == 2,
        "unsupported Harbor trial lock schema"
    );
    let digest = required_string(lock.field("/task/digest"), "Harbor pinned task digest")?;
    ensure!(
        digest.strip_prefix("sha256:").is_some_and(
            |value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        ),
        "Harbor task must have a pinned SHA-256 digest"
    );
    if lock.field("/task/type") == "git" {
        let commit = required_string(
            lock.field("/task/git_commit_id"),
            "Harbor pinned Git commit",
        )?;
        ensure!(
            commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Harbor Git task must pin a full commit"
        );
    }
    ensure!(
        lock.field("/install_only") != true && lock.field("/source_trial").is_null(),
        "Harbor install-only and regraded trials cannot compare harness execution"
    );
    ensure!(
        lock.field("/verifier/disable") != true,
        "Harbor verifier is disabled"
    );
    ensure!(
        lock.field("/user_agent").is_null(),
        "Harbor simulated-user trials are not supported by this comparison"
    );
    ensure!(
        lock.field("/agent/load_trajectory").is_null(),
        "Harbor loaded trajectories cannot compare fresh harness execution"
    );
    Ok(())
}

fn trial_settings(lock: &Value) -> Result<Value> {
    let mut settings = lock
        .as_object()
        .ok_or_else(|| eyre!("Harbor trial lock must be an object"))?
        .clone();
    settings.remove("task");
    settings.remove("agent");
    let limits: serde_json::Map<String, Value> = [
        "override_timeout_sec",
        "override_setup_timeout_sec",
        "max_timeout_sec",
        "extra_allowed_hosts",
        "n_concurrent",
    ]
    .into_iter()
    .map(|key| {
        (
            key.to_owned(),
            lock.field("/agent")
                .get(key)
                .unwrap_or(&Value::Null)
                .clone(),
        )
    })
    .collect();
    settings.insert(String::from("agent_limits"), Value::Object(limits));
    Ok(Value::Object(settings))
}

fn proxy_metadata(
    path: &Path,
    lock: &Value,
    require_controller: bool,
) -> Result<(
    Option<crate::ProxyMetrics>,
    Option<String>,
    Option<HarborModelSettings>,
)> {
    let metrics_path = path.join("kraai-controller/proxy-metrics.json");
    if !metrics_path.is_file() {
        ensure!(
            !require_controller,
            "Harbor profile adapter is missing private controller proxy metrics"
        );
        if profile_adapter(lock) {
            return Ok((None, None, None));
        }
        let settings = lock
            .field("/agent/kwargs/reasoning_effort")
            .as_str()
            .map(|effort| HarborModelSettings {
                model: lock
                    .field("/agent/model_name")
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                reasoning_effort: Some(effort.to_owned()),
                service_tier: None,
            });
        return Ok((None, None, settings));
    }
    let mut metrics: crate::ProxyMetrics = serde_json::from_value(read_json(&metrics_path)?)?;
    if let Some(accounting) =
        crate::accounting::load_accounting(&path.join("kraai-controller/request-accounting.json"))?
    {
        metrics.accounting = Some(accounting);
        metrics.accounting_error = None;
    }
    let identity = read_json(&path.join("kraai-controller/proxy-identity.json"))?;
    let identity = normalized_proxy_identity(&identity)?;
    let events = fs::read_to_string(path.join("kraai-controller/proxy.events.jsonl"))
        .wrap_err("read Harbor controller proxy events")?;
    let mut observed = None;
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value = serde_json::from_str(line)?;
        let Some(model) = event.field("/model").as_str() else {
            continue;
        };
        let settings = HarborModelSettings {
            model: model.to_owned(),
            reasoning_effort: event.field("/reasoning_effort").as_str().map(str::to_owned),
            service_tier: event.field("/service_tier").as_str().map(str::to_owned),
        };
        ensure!(
            observed
                .as_ref()
                .is_none_or(|previous| previous == &settings),
            "Harbor trial changed actual model settings during execution"
        );
        observed = Some(settings);
    }
    Ok((Some(metrics), Some(json_hash(&identity)?), observed))
}

fn profile_adapter(lock: &Value) -> bool {
    lock.field("/agent/import_path")
        .as_str()
        .is_some_and(|path| path.starts_with("kraai_harbor."))
        || !lock.field("/agent/kwargs/spec_path").is_null()
}

fn normalized_proxy_identity(identity: &Value) -> Result<Value> {
    ensure!(
        identity.field("/transport_revision").as_u64().is_some()
            && identity
                .field("/max_requests")
                .as_u64()
                .is_some_and(|count| count > 0),
        "invalid Harbor proxy identity"
    );
    let mut paths = identity
        .field("/allowed_paths")
        .as_array()
        .ok_or_else(|| eyre!("Harbor proxy allowed paths are missing"))?
        .iter()
        .map(|value| required_string(value, "Harbor proxy allowed path"))
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    paths.dedup();
    Ok(serde_json::json!({
        "transport_revision": identity.field("/transport_revision"), "max_requests": identity.field("/max_requests"),
        "kind": required_string(identity.field("/kind"), "Harbor proxy kind")?,
        "upstream": required_string(identity.field("/upstream"), "Harbor proxy upstream")?, "allowed_paths": paths,
    }))
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).wrap_err_with(|| format!("read {}", path.display()))?)
        .wrap_err_with(|| format!("parse {}", path.display()))
}

fn required_string(value: &Value, name: &str) -> Result<String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| eyre!("missing {name}"))
}

fn json_hash(value: &Value) -> Result<String> {
    Ok(crate::cache::hash_chunks(&[serde_json::to_vec(value)?]))
}
