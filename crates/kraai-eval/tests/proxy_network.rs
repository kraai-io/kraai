// Clippy only applies its allow-*-in-tests settings to helpers under a `cfg(test)` ancestor.
#![cfg(test)]
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;

use kraai_eval::{KraaiProviderConfigRequest, ModelProxyRequest, RunRequest, RunStatus};
use support::{find_program, temporary_directory, user_scope_available};

mod support;

const RUNNER_SCRIPT: &str = r#"
set -u
authority=${OPENAI_BASE_URL#http://}
base_path=/${authority#*/}
authority=${authority%%/*}
exec 3<>"/dev/tcp/${authority%:*}/${authority##*:}" || exit 10
printf 'POST %s/chat/completions HTTP/1.1\r\nHost: %s\r\nAuthorization: Bearer %s\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}' "$base_path" "$authority" "$OPENAI_API_KEY" >&3
IFS= read -r status <&3 || exit 11
exec 3<&-
case $status in
  "HTTP/1.1 200"*) ;;
  *) printf 'proxy answered %s\n' "$status" >&2; exit 12 ;;
esac
if (exec 4<>"/dev/tcp/127.0.0.1/$HOST_PORT") 2>/dev/null; then
  host_loopback=reachable
else
  host_loopback=unreachable
fi
[ "$host_loopback" = "$EXPECTED_HOST_LOOPBACK" ] || { echo "host loopback was $host_loopback" >&2; exit 13; }
if [ "$EXPECTED_HOST_LOOPBACK" = unreachable ] && (exec 5<>/dev/tcp/1.1.1.1/443) 2>/dev/null; then
  echo "the internet was reachable" >&2
  exit 14
fi
"#;

#[test]
fn offline_runner_reaches_only_the_model_proxy() {
    run_with_network("disabled", "unreachable");
}

#[test]
fn networked_runner_keeps_host_network_access() {
    run_with_network("enabled", "reachable");
}

fn run_with_network(network: &str, host_loopback: &str) {
    let Some(bash) = find_program("bash") else {
        return;
    };
    if find_program("bwrap").is_none() || !user_scope_available() {
        return;
    }
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let upstream_port = upstream.local_addr().unwrap().port();
    std::thread::spawn(move || serve_upstream(&upstream));

    let root = temporary_directory(&format!("proxy-network-{network}"));
    let task_dir = root.join("task");
    fs::create_dir_all(task_dir.join("fixture")).unwrap();
    fs::write(
        task_dir.join("fixture/probe.sh"),
        format!(
            "HOST_PORT={upstream_port}\nEXPECTED_HOST_LOOPBACK={host_loopback}\n{RUNNER_SCRIPT}"
        ),
    )
    .unwrap();
    fs::write(
        task_dir.join("task.toml"),
        format!(
            r#"schema_version = 1
id = "proxy-network"
prompt = "Call the model."

[source]
directory = "fixture"

[runner]
network = "{network}"
timeout_seconds = {timeout}

[[grader.commands]]
command = ["{bash}", "-c", "true"]
timeout_seconds = {timeout}
"#,
            bash = bash.display(),
            timeout = kraai_test_support::HANG_TIMEOUT.as_secs(),
        ),
    )
    .unwrap();
    let providers = root.join("providers.toml");
    fs::write(
        &providers,
        format!(
            "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\napi_key = 'upstream-secret'\nbase_url = 'http://127.0.0.1:{upstream_port}/v1'\n"
        ),
    ).unwrap();
    let model_proxy = ModelProxyRequest::kraai_provider(
        KraaiProviderConfigRequest::new(providers, Some(String::from("local"))),
        4,
    )
    .unwrap();
    let eval = root.join("eval");
    fs::create_dir_all(&eval).unwrap();
    let result = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("task.toml"),
        runner_program: bash,
        runner_args: vec![String::from("probe.sh")],
        runner_version: String::from("proxy-network"),
        harness_name: Some(String::from("proxy-network")),
        model_label: None,
        attempt: 1,
        eval_root: eval.clone(),
        artifact_path: PathBuf::from("fixture/proxy-network/attempt-1"),
        staging_dir: eval.join(".cache/staging/fixture"),
        model_proxy: Some(model_proxy),
        proxy_bridge: Some(PathBuf::from(env!("CARGO_BIN_EXE_kraai-eval"))),
        kraai_provider_config: None,
        progress: None,
    })
    .unwrap();
    let artifacts = eval.join(&result.artifact_path);
    let stderr = fs::read_to_string(artifacts.join("runner.stderr.log")).unwrap_or_default();
    assert!(
        result.status == RunStatus::Passed,
        "status={:?}, runner={:?}, stderr={stderr}",
        result.status,
        result.runner
    );
    assert!(
        result
            .metrics
            .proxy
            .as_ref()
            .map(|metrics| metrics.requests)
            == Some(1),
        "the proxy did not record the runner request"
    );
}

fn serve_upstream(listener: &TcpListener) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            return;
        };
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => request.extend_from_slice(buffer.get(..count).unwrap_or_default()),
            }
        }
        if request.is_empty() {
            continue;
        }
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
    }
}
