use std::sync::atomic::Ordering;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, PROFILE_ID, PROFILE_PROMPT, prompt};

#[tokio::test]
async fn workspace_profile_configuration_persists_and_controls_the_model_prompt() -> Result<()> {
    let mut harness = Harness::new(vec!["Profile applied".into()]).await?;
    harness.install_profile().await?;
    harness.initialize().await?;
    let created = harness
        .request(
            1,
            "session/new",
            json!({"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    let response = created
        .last()
        .ok_or_else(|| eyre!("missing session response"))?;
    let session = response
        .pointer("/result/sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing session ID"))?
        .to_owned();
    assert_profile_options(response);
    assert_commands(&created);

    let invalid = harness
        .request(
            2,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"profile","value":"missing-profile"}),
        )
        .await?;
    assert!(
        invalid
            .last()
            .is_some_and(|value| value.get("error").is_some())
    );
    let selected = harness
        .request(
            3,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"profile","value":PROFILE_ID}),
        )
        .await?;
    assert_eq!(
        selected
            .last()
            .and_then(|value| value.pointer("/result/configOptions/1/currentValue")),
        Some(&json!(PROFILE_ID))
    );
    assert!(selected.iter().any(|value| {
        value.pointer("/params/update/sessionUpdate") == Some(&json!("config_option_update"))
            && value.pointer("/params/update/configOptions/1/currentValue")
                == Some(&json!(PROFILE_ID))
    }));

    harness.restart().await?;
    harness.initialize().await?;
    let loaded = harness
        .request(
            4,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    let response = loaded
        .last()
        .ok_or_else(|| eyre!("missing load response"))?;
    assert_profile_options(response);
    assert_eq!(
        response.pointer("/result/configOptions/1/currentValue"),
        Some(&json!(PROFILE_ID))
    );
    assert_commands(&loaded);
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 0);

    let generated = harness
        .request(5, "session/prompt", prompt(&session))
        .await?;
    assert_eq!(
        generated
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    let payloads = harness.provider_payloads.lock().await.clone();
    assert_eq!(payloads.len(), 1);
    assert!(
        payloads
            .first()
            .and_then(|value| value.get("messages"))
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role") == Some(&json!("system"))
                        && message
                            .get("content")
                            .is_some_and(|content| content.to_string().contains(PROFILE_PROMPT))
                })
            })
    );
    harness.stop().await
}

fn assert_profile_options(response: &Value) {
    assert_eq!(
        response.pointer("/result/configOptions/0/id"),
        Some(&json!("model"))
    );
    assert_eq!(
        response.pointer("/result/configOptions/1/id"),
        Some(&json!("profile"))
    );
    assert!(
        response
            .pointer("/result/configOptions/1/options")
            .and_then(Value::as_array)
            .is_some_and(|options| {
                options
                    .iter()
                    .any(|option| option.get("value") == Some(&json!(PROFILE_ID)))
            })
    );
}

fn assert_commands(messages: &[Value]) {
    let commands = messages
        .iter()
        .find(|value| {
            value.pointer("/params/update/sessionUpdate")
                == Some(&json!("available_commands_update"))
        })
        .and_then(|value| value.pointer("/params/update/availableCommands"))
        .and_then(Value::as_array);
    let mut names = commands
        .into_iter()
        .flatten()
        .filter_map(|command| command.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["continue", "undo"]);
}
