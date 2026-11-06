use std::fs;
use std::path::Path;
use std::process::ExitCode;

use color_eyre::eyre::{Result, ensure};
use kraai_eval::{
    KraaiProviderConfigRequest, ProgressReporter, RunRequest, RunStatus, SuiteRequest,
};

use super::display::{ProgressDisplay, attempt_costs, format_suite_summary, priced_summary_json};
use super::{Prepared, RunArgs, exit_code, print_json, tasks};

pub(super) fn run(
    args: RunArgs,
    prepared: Prepared,
    tasks_dir: &Path,
    json: bool,
) -> Result<ExitCode> {
    let Prepared {
        root,
        profile,
        harness,
        directory,
        price,
        prices,
    } = prepared;
    let mut tasks = tasks::discover(tasks_dir)?
        .into_values()
        .collect::<Vec<_>>();
    if let Some(count) = args.tasks {
        ensure!(
            usize::try_from(count).is_ok_and(|count| count <= tasks.len()),
            "--tasks must be at most {}, the number of in-house tasks",
            tasks.len()
        );
        tasks.truncate(usize::try_from(count)?);
    }
    for task in &tasks {
        let mut manifest = task.manifest.clone();
        manifest.resolve_source_revision(task.path.parent().unwrap_or_else(|| Path::new(".")))?;
    }
    if args.dry_run {
        print_json(&serde_json::json!({
            "benchmark": args.benchmark,
            "tasks": tasks.iter().map(|task| &task.manifest.id).collect::<Vec<_>>(),
            "attempts": args.attempts,
            "harness": harness.name,
            "program": harness.program,
            "args": harness.args,
            "version": harness.version,
            "model": harness.model,
            "reasoning": harness.reasoning,
            "price": price,
            "results": directory.path,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = super::lock(&root, &directory)?;
    let staging = root.staging(&directory);
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let provider_config = profile
        .sanitize_kraai_provider
        .then(|| {
            super::provider_config_path(&args)
                .map(|path| KraaiProviderConfigRequest::new(path, args.provider.clone()))
        })
        .transpose()?;
    let model_proxy = profile.model_proxy(provider_config.as_ref())?;
    let progress = ProgressReporter::new();
    let display = ProgressDisplay::start(progress.clone());
    let model_label = harness.model_label();
    let mut runs = Vec::new();
    for task in tasks {
        let task_dir = directory
            .relative
            .join(kraai_eval::path_segment(&task.manifest.id, "task"));
        for attempt in 1..=args.attempts {
            runs.push(RunRequest {
                task_path: task.path.clone(),
                runner_program: harness.program.clone(),
                runner_args: harness.args.clone(),
                runner_version: harness.version.clone(),
                harness_name: Some(harness.name.clone()),
                model_label: Some(model_label.clone()),
                attempt,
                eval_root: root.path().to_path_buf(),
                artifact_path: task_dir.join(format!("attempt-{attempt}")),
                staging_dir: staging.clone(),
                model_proxy: model_proxy.clone(),
                kraai_provider_config: provider_config.clone(),
                progress: Some(progress.clone()),
            });
        }
    }
    let result = kraai_eval::run_suite(&SuiteRequest {
        runs,
        eval_root: root.path().to_path_buf(),
        harness_dir: directory.relative.clone(),
    });
    display.finish();
    let outcome = result?;
    let costs = attempt_costs(&outcome.results, &prices);
    let result = outcome.summary;
    if json {
        print_json(&priced_summary_json(&result, &costs)?)?;
    } else {
        println!("{}", format_suite_summary(&result, root.path(), &costs));
        for run in &result.runs {
            println!(
                "  {} attempt {}: {}",
                run.task_id.as_deref().unwrap_or("unknown task"),
                run.attempt,
                run.error.clone().unwrap_or_else(|| format!(
                    "{:?}",
                    run.status.as_ref().unwrap_or(&RunStatus::ControllerFailed)
                ))
            );
        }
    }
    Ok(exit_code(result.passed_runs == result.requested_runs))
}

pub(super) fn check(tasks_dir: &Path, selectors: &[String], json: bool) -> Result<ExitCode> {
    let tasks = tasks::select(tasks_dir, selectors)?;
    let mut results = Vec::new();
    for task in tasks {
        results.push(kraai_eval::validate_task(&task.path)?);
    }
    let passed = results.iter().all(kraai_eval::TaskValidation::passed);
    if json {
        print_json(&results)?;
    } else {
        for result in results {
            println!(
                "{}: {}",
                result.task_id,
                if result.passed() {
                    "grader checks passed"
                } else {
                    "GRADER CHECKS FAILED"
                }
            );
            for diagnostic in result.diagnostics {
                println!("{diagnostic}");
            }
        }
    }
    Ok(exit_code(passed))
}
