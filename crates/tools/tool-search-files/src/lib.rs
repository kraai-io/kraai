#![forbid(unsafe_code)]

use std::path::Path;

use async_trait::async_trait;
use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
use ignore::WalkBuilder;
use serde::Serialize;
use tool_core::{ToolCallResult, ToolContext, TypedTool, assess_read_path, resolve_tool_path};
use toon_schema::toon_tool;
use types::ToolCallAssessment;

const MAX_MATCHES: usize = 100;

#[derive(Clone, Copy)]
pub struct SearchFilesTool;

toon_tool! {
    name: "search_files",
    description: "Search files recursively using ripgrep and return matching lines",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct SearchFilesToolArgs {
            #[toon_schema(description = "Regex pattern to search for")]
            query: String,

            #[toon_schema(
                description = "Optional file or directory path to search. Uses the workspace root when omitted"
            )]
            path: Option<String>,
        }
    },
    root: SearchFilesToolArgs,
    examples: [
        { query: "fn name\\(", path: "crates/agent-runtime" },
        { query: "TODO" },
    ]
}

#[derive(Serialize)]
struct SearchFilesToolOutput {
    query: String,
    path: String,
    matches: Vec<SearchMatch>,
    truncated: bool,
    match_count: usize,
}

#[derive(Clone, Serialize)]
struct SearchMatch {
    path: String,
    line_number: u64,
    line_text: String,
}

#[derive(Default)]
struct SearchState {
    matches: Vec<SearchMatch>,
    truncated: bool,
}

#[async_trait]
impl TypedTool for SearchFilesTool {
    type Args = SearchFilesToolArgs;

    fn name(&self) -> &'static str {
        SearchFilesToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        SearchFilesToolArgs::toon_schema()
    }

    fn assess(&self, args: &Self::Args, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let raw_path = args.path.clone().unwrap_or_else(|| String::from("."));
        assess_read_path(
            &ctx.global_config.workspace_dir,
            &raw_path,
            "Searches workspace path",
            "Searches path outside workspace",
        )
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolCallResult {
        let raw_path = args.path.unwrap_or_else(|| String::from("."));
        let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, &raw_path);
        let metadata = match std::fs::metadata(resolved.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                return ToolCallResult::error(format!(
                    "unable to access search path {}: {}",
                    resolved.path().display(),
                    error
                ));
            }
        };

        let matcher = match RegexMatcher::new(&args.query) {
            Ok(matcher) => matcher,
            Err(error) => return ToolCallResult::error(format!("invalid regex: {error}")),
        };

        let mut state = SearchState::default();

        let search_result = if metadata.is_file() {
            search_file(resolved.path(), &matcher, &mut state)
        } else if metadata.is_dir() {
            search_directory(resolved.path(), &matcher, &mut state)
        } else {
            return ToolCallResult::error(format!(
                "path is neither a file nor a directory: {}",
                resolved.path().display()
            ));
        };

        if let Err(error) = search_result {
            return ToolCallResult::error(format!(
                "unable to search path {}: {}",
                resolved.path().display(),
                error
            ));
        }

        let output = SearchFilesToolOutput {
            query: args.query,
            path: resolved.path().display().to_string(),
            match_count: state.matches.len(),
            matches: state.matches,
            truncated: state.truncated,
        };

        ToolCallResult::success(output)
    }

    fn describe(&self, args: &Self::Args) -> String {
        let target = args.path.clone().unwrap_or_else(|| String::from("."));
        format!("Search files in {} for {}", target, args.query)
    }
}

fn search_directory(
    dir: &Path,
    matcher: &RegexMatcher,
    state: &mut SearchState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = WalkBuilder::new(dir);
    builder.standard_filters(true);
    builder.hidden(true);
    builder.require_git(false);

    for entry in builder.build() {
        let entry = entry?;
        if !entry
            .file_type()
            .map(|file_type| file_type.is_file())
            .unwrap_or(false)
        {
            continue;
        }

        if let Err(error) = search_file(entry.path(), matcher, state) {
            if error.to_string().contains("invalid utf-8") {
                continue;
            }
            return Err(error);
        }
        if state.truncated {
            break;
        }
    }

    Ok(())
}

fn search_file(
    path: &Path,
    matcher: &RegexMatcher,
    state: &mut SearchState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .build();

    let path_string = path.display().to_string();
    let mut sink = UTF8(|line_number: u64, line: &str| {
        if state.matches.len() >= MAX_MATCHES {
            state.truncated = true;
            return Ok(false);
        }

        if matcher.is_match(line.as_bytes())? {
            state.matches.push(SearchMatch {
                path: path_string.clone(),
                line_number,
                line_text: line.trim_end_matches(['\r', '\n']).to_string(),
            });
        }

        if state.matches.len() >= MAX_MATCHES {
            state.truncated = true;
            Ok(false)
        } else {
            Ok(true)
        }
    });

    searcher.search_path(matcher, path, &mut sink)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use tool_core::{ToolContext, ToolOutput, TypedTool};
    use types::{ExecutionPolicy, RiskLevel, ToolCallGlobalConfig, ToolStateSnapshot};

    use super::{MAX_MATCHES, SearchFilesTool, SearchFilesToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        ToolCallGlobalConfig {
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    fn tool_context<'a>(
        config: &'a ToolCallGlobalConfig,
        snapshot: &'a ToolStateSnapshot,
    ) -> ToolContext<'a> {
        ToolContext {
            global_config: config,
            tool_state_snapshot: snapshot,
        }
    }

    fn make_temp_dir(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agent-tool-search-files-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    fn search_args(
        query: impl Into<String>,
        path: Option<impl Into<String>>,
    ) -> SearchFilesToolArgs {
        SearchFilesToolArgs {
            query: query.into(),
            path: path.map(Into::into),
        }
    }

    #[tokio::test]
    async fn searches_workspace_root_when_path_is_omitted() {
        let workspace_dir = make_temp_dir("searches_workspace_root_when_path_is_omitted");
        fs::write(workspace_dir.join("root.txt"), "alpha\nneedle\n").expect("write root file");

        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("needle", Option::<String>::None),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let matches = data["matches"].as_array().expect("matches array");
                assert_eq!(matches.len(), 1);
                assert_eq!(matches[0]["line_number"].as_u64(), Some(2));
                assert_eq!(matches[0]["line_text"].as_str(), Some("needle"));
                assert_eq!(
                    data["path"].as_str(),
                    Some(workspace_dir.to_string_lossy().as_ref())
                );
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn searches_specific_directory_recursively() {
        let workspace_dir = make_temp_dir("searches_specific_directory_recursively");
        let nested = workspace_dir.join("nested");
        fs::create_dir_all(nested.join("deep")).expect("create nested dirs");
        fs::write(nested.join("deep").join("match.txt"), "fn hello()\n").expect("write file");

        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("fn\\s+hello\\(", Some("nested")),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let matches = data["matches"].as_array().expect("matches array");
                assert_eq!(matches.len(), 1);
                assert!(
                    matches[0]["path"]
                        .as_str()
                        .expect("path")
                        .ends_with("nested/deep/match.txt")
                );
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn searches_single_file_path() {
        let workspace_dir = make_temp_dir("searches_single_file_path");
        fs::write(workspace_dir.join("single.txt"), "zero\none\ntwo\n").expect("write file");

        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("one", Some("single.txt")),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let matches = data["matches"].as_array().expect("matches array");
                assert_eq!(matches.len(), 1);
                assert_eq!(matches[0]["line_number"].as_u64(), Some(2));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn skips_hidden_ignored_and_binary_files() {
        let workspace_dir = make_temp_dir("skips_hidden_ignored_and_binary_files");
        fs::write(workspace_dir.join(".gitignore"), "ignored.txt\n").expect("write gitignore");
        fs::write(workspace_dir.join(".hidden.txt"), "needle\n").expect("write hidden file");
        fs::write(workspace_dir.join("ignored.txt"), "needle\n").expect("write ignored file");
        fs::write(workspace_dir.join("visible.txt"), "needle\n").expect("write visible file");
        fs::write(workspace_dir.join("binary.bin"), [0, 159, 146, 150, b'n'])
            .expect("write binary");

        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("needle", Option::<String>::None),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let matches = data["matches"].as_array().expect("matches array");
                assert_eq!(matches.len(), 1);
                assert!(
                    matches[0]["path"]
                        .as_str()
                        .expect("path")
                        .ends_with("visible.txt")
                );
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn skips_non_utf8_files_without_aborting_directory_search() {
        let workspace_dir = make_temp_dir("skips_non_utf8_files_without_aborting_directory_search");
        fs::write(workspace_dir.join("valid.txt"), "needle\n").expect("write valid file");
        fs::write(
            workspace_dir.join("broken.txt"),
            [0x80, b'n', b'e', b'e', b'd', b'l', b'e'],
        )
        .expect("write invalid utf8 file");

        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("needle", Option::<String>::None),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let matches = data["matches"].as_array().expect("matches array");
                assert_eq!(matches.len(), 1);
                assert!(matches[0]["path"]
                    .as_str()
                    .expect("path")
                    .ends_with("valid.txt"));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn returns_error_for_missing_path() {
        let workspace_dir = make_temp_dir("returns_error_for_missing_path");
        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("needle", Some("missing")),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Error { message } => {
                assert!(message.contains("unable to access search path"));
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn truncates_after_maximum_matches() {
        let workspace_dir = make_temp_dir("truncates_after_maximum_matches");
        let content = (0..(MAX_MATCHES + 5))
            .map(|index| format!("needle-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(workspace_dir.join("many.txt"), content).expect("write file");

        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("needle-", Option::<String>::None),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let matches = data["matches"].as_array().expect("matches array");
                assert_eq!(matches.len(), MAX_MATCHES);
                assert_eq!(data["truncated"].as_bool(), Some(true));
                assert_eq!(data["match_count"].as_u64(), Some(MAX_MATCHES as u64));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assess_marks_workspace_path_as_read_only() {
        let workspace_dir = make_temp_dir("assess_marks_workspace_path_as_read_only");
        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let assessment = tool.assess(
            &search_args("needle", Option::<String>::None),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);
        assert_eq!(
            assessment.policy,
            ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace)
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assess_marks_outside_workspace_path_as_outside() {
        let workspace_dir = make_temp_dir("assess_marks_outside_workspace_path_as_outside");
        let outside_dir = make_temp_dir("assess_outside_target");
        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let assessment = tool.assess(
            &search_args("needle", Some(outside_dir.to_string_lossy().to_string())),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyOutsideWorkspace);

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }

    #[tokio::test]
    async fn returns_error_for_invalid_regex() {
        let workspace_dir = make_temp_dir("returns_error_for_invalid_regex");
        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                search_args("(", Option::<String>::None),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Error { message } => {
                assert!(message.contains("invalid regex"));
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assess_marks_relative_parent_path_as_outside() {
        let workspace_dir = make_temp_dir("assess_marks_relative_parent_path_as_outside");
        let outside_dir = make_temp_dir("search_relative_outside_target");
        let relative_path = format!(
            "../{}",
            outside_dir
                .file_name()
                .expect("outside dir name")
                .to_string_lossy()
        );
        let tool = SearchFilesTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let assessment = tool.assess(
            &search_args("needle", Some(relative_path)),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyOutsideWorkspace);

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }
}
