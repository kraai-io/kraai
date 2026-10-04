use color_eyre::eyre::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const MAX_REASONING_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Reasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Vec<Map<String, Value>>>,
    #[serde(skip)]
    accumulated_bytes: usize,
}

impl Reasoning {
    pub fn is_empty(&self) -> bool {
        self.reasoning_content.is_none()
            && self.reasoning.is_none()
            && self.reasoning_text.is_none()
            && self.reasoning_details.as_ref().is_none_or(Vec::is_empty)
    }

    pub fn append(&mut self, delta: Self) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&delta)?.len();
        ensure!(
            self.accumulated_bytes.saturating_add(bytes) <= MAX_REASONING_BYTES,
            "Reasoning exceeds size limit"
        );
        self.accumulated_bytes += bytes;
        for (target, fragment) in [
            (&mut self.reasoning_content, delta.reasoning_content),
            (&mut self.reasoning, delta.reasoning),
            (&mut self.reasoning_text, delta.reasoning_text),
        ] {
            if let Some(fragment) = fragment {
                let target = target.get_or_insert_with(String::new);
                target.push_str(&fragment);
            }
        }
        if let Some(details) = delta.reasoning_details {
            let target = self.reasoning_details.get_or_insert_with(Vec::new);
            for detail in details {
                append_detail(target, detail)?;
            }
        }
        Ok(())
    }
}

fn append_detail(details: &mut Vec<Map<String, Value>>, detail: Map<String, Value>) -> Result<()> {
    let content_field = match detail.get("type").and_then(Value::as_str) {
        Some("reasoning.text") => Some("text"),
        Some("reasoning.summary") => Some("summary"),
        _ => None,
    };
    let previous = details.last_mut().filter(|previous| {
        content_field.is_some()
            && previous.get("type") == detail.get("type")
            && ["id", "index", "format"].into_iter().all(|key| {
                let previous = previous.get(key).filter(|value| !value.is_null());
                let current = detail.get(key).filter(|value| !value.is_null());
                previous.is_none() || current.is_none() || previous == current
            })
    });
    if let Some(previous) = previous {
        for (key, value) in detail {
            if value.is_null() && previous.contains_key(&key) {
                continue;
            }
            match previous.get_mut(&key).filter(|value| !value.is_null()) {
                Some(Value::String(target)) if Some(key.as_str()) == content_field => {
                    let fragment = value.as_str().ok_or_else(|| {
                        color_eyre::eyre::eyre!("Reasoning detail {key} is not a string")
                    })?;
                    target.push_str(fragment);
                }
                Some(Value::String(target)) if key == "signature" && target.is_empty() => {
                    *target = value
                        .as_str()
                        .ok_or_else(|| {
                            color_eyre::eyre::eyre!("Reasoning detail signature is not a string")
                        })?
                        .to_owned();
                }
                Some(target) => ensure!(
                    *target == value,
                    "Reasoning detail metadata changed during streaming: {key}"
                ),
                None => {
                    previous.insert(key, value);
                }
            }
        }
    } else {
        details.push(detail);
    }
    Ok(())
}
