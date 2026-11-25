use std::fmt::Write;

use kraai_types::TokenRates;
use serde_json::json;

use super::*;

const GROQ: &str = "https://api.groq.com/openai/v1";
const PROXY: &str = "http://proxy.test/v1";

fn providers() -> Value {
    let mut providers: Value =
        serde_json::from_str(include_str!("fixtures/models-dev.json")).unwrap();
    let edges = json!({
        "edge": {"api": "https://edge.test/v1/", "name": "Edge Cases", "models": {
            "unknown-dimension": {"cost": {"input": 1, "output": 2, "batch_input": 0.5}},
            "unknown-tier-type": {"cost": {"input": 1, "output": 2,
                "tiers": [{"tier": {"type": "tokens", "size": 10}, "input": 2, "output": 3}]}},
            "unknown-tier-key": {"cost": {"input": 1, "output": 2,
                "tiers": [{"tier": {"size": 10, "unit": "tokens"}, "input": 2, "output": 3}]}},
            "tiers-not-array": {"cost": {"input": 1, "output": 2, "tiers": {}}},
            "invalid-rate": {"cost": {"input": 1, "output": 2, "cache_read": "free"}},
            "negative-tier": {"cost": {"input": 1, "output": 2,
                "tiers": [{"tier": {"size": 1000}, "input": -1, "output": 3}]}},
            "missing-input": {"cost": {"output": 2,
                "tiers": [{"tier": {"size": 1000}, "input": 4, "output": 5}]}},
            "null-over-200k": {"cost": {"input": 1, "output": 2, "context_over_200k": null}},
            "over-200k": {"cost": {"input": 1, "output": 2, "input_audio": 7,
                "context_over_200k": {"input": 3, "output": 4, "reasoning": 5, "extra": 1}}},
            "tiers-win": {"cost": {"input": 1, "output": 2, "tiers": [],
                "context_over_200k": {"input": 3, "output": 4}}},
            "equal-tiers": {"cost": {"input": 1, "output": 2, "tiers": [
                {"tier": {"size": 1000}, "input": 3, "output": 4},
                {"tier": {"type": "context", "size": 1000}, "input": 5, "output": 6},
                {"tier": {"size": 0}, "input": 7, "output": 8},
                {"tier": {"size": 150000}, "input": 9, "output": 10, "cache_write": 11}
            ]}},
            "null-cost": {"cost": null},
            "scalar-cost": {"cost": 5},
            "zero-limits": {"limit": {"context": 0, "output": 0}, "modalities": {"input": []}},
            "bare": {}
        }},
        "same-name-a": {"name": "Shared Name", "models": {"shared": {"name": "A"}}},
        "same-name-b": {"name": "shared NAME", "models": {"shared": {"name": "B"}}}
    });
    providers
        .as_object_mut()
        .unwrap()
        .extend(edges.as_object().unwrap().clone());
    providers
}

type Lookup = (
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    Option<&'static str>,
);

fn lookups() -> Vec<Lookup> {
    let mut lookups = vec![
        (Some("openai"), None, "gpt-5", None),
        (Some("openai"), Some(PROXY), "gpt-5.4", None),
        (Some("openai"), None, "text-embedding-3-small", None),
        (Some("anthropic"), None, "claude-sonnet-4-5", None),
        (Some("azure"), None, "gpt-5", None),
        (Some("openrouter"), None, "openai/gpt-5.6-sol-pro", None),
        (
            Some("openrouter"),
            None,
            "anthropic/claude-sonnet-4.5",
            None,
        ),
        (Some("openrouter"), None, "minimax/minimax-m2.7", None),
        (Some("helicone"), None, "llama-3.3-70b-versatile", None),
        (Some("github-copilot"), None, "grok-4.7", None),
        (Some("github-copilot"), None, "claude-haiku-5.5", None),
        (Some("missing"), Some(GROQ), "llama-3.3-70b-versatile", None),
        (Some("groq"), None, "missing-model", None),
        (
            None,
            Some("https://openrouter.ai/api/v1"),
            "openai/gpt-5",
            None,
        ),
        (
            None,
            Some("https://openrouter.ai/api/v1/"),
            "openai/gpt-oss-120b",
            None,
        ),
        (
            None,
            Some("https://ai-gateway.helicone.ai/v1"),
            "gpt-5",
            None,
        ),
        (
            None,
            Some("https://api.githubcopilot.com"),
            "gpt-5.6-sol",
            None,
        ),
        (
            None,
            Some("https://api.code.umans.ai/v1"),
            "umans-coder",
            None,
        ),
        (
            None,
            Some("https://api.code.umans.ai/v1"),
            "umans-coder",
            Some("Umans AI"),
        ),
        (
            None,
            Some("https://api.meta.ai/v1"),
            "muse-spark-1.3",
            Some("openai"),
        ),
        (None, Some(GROQ), "llama-3.3-70b-versatile", None),
        (
            None,
            Some("https://api.groq.com/openai/v1/"),
            "qwen/qwen3.6-27b",
            None,
        ),
        (None, Some(GROQ), "whisper-large-v3", None),
        (
            None,
            Some("https://api.groq.com/other"),
            "qwen/qwen3.6-27b",
            None,
        ),
        (None, Some(GROQ), "gpt-5", None),
        (None, None, "gpt-5", None),
        (None, None, "claude-sonnet-4-5", None),
        (None, None, "openai/gpt-5", None),
        (None, None, "openai/gpt-oss-120b", None),
        (None, None, "openai/gpt-5.6-sol-pro", None),
        (None, None, "minimax/minimax-m2.7", None),
        (None, None, "minimax/MiniMax-M2.7", None),
        (None, None, "anthropic/claude-sonnet-4.5", None),
        (None, None, "llama-3.3-70b-versatile", None),
        (None, None, "umans-coder", None),
        (None, None, "muse-spark-1.3", None),
        (None, None, "grok-4.7", None),
        (None, None, "missing-model", None),
        (None, Some(PROXY), "gpt-5", Some("openai")),
        (None, Some(PROXY), "gpt-5", Some("OpenAI")),
        (None, Some(PROXY), "gpt-5.4", Some(" OPENAI ")),
        (None, Some(PROXY), "claude-sonnet-4-5", Some("Anthropic")),
        (None, Some(PROXY), "openai/gpt-oss-120b", Some("groq")),
        (None, Some(PROXY), "gpt-5", Some("azure")),
        (
            None,
            Some(PROXY),
            "minimax/minimax-m2.7",
            Some("OpenRouter"),
        ),
        (None, Some(PROXY), "shared", Some("shared name")),
        (None, Some(PROXY), "shared", Some("same-name-b")),
        (None, Some(PROXY), "gpt-5", Some("nobody")),
        (Some("openai"), None, "gpt-5", Some("anthropic")),
    ];
    for model in [
        "unknown-dimension",
        "unknown-tier-type",
        "unknown-tier-key",
        "tiers-not-array",
        "invalid-rate",
        "negative-tier",
        "missing-input",
        "null-over-200k",
        "over-200k",
        "tiers-win",
        "equal-tiers",
        "null-cost",
        "scalar-cost",
        "zero-limits",
        "bare",
    ] {
        lookups.push((None, Some("https://edge.test/v1"), model, None));
    }
    lookups
}

fn usages() -> Vec<TokenUsage> {
    [
        (0, 0, 0),
        (100, 0, 0),
        (900, 100, 1),
        (150_000, 0, 0),
        (200_000, 0, 1),
        (300_000, 4, 5),
    ]
    .into_iter()
    .map(
        |(input_tokens, cache_read_tokens, cache_write_tokens)| TokenUsage {
            input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            output_tokens: 10,
            reasoning_tokens: 3,
            ..Default::default()
        },
    )
    .collect()
}

#[tokio::test]
async fn sql_resolution_matches_the_in_memory_catalog_on_a_models_dev_fixture() {
    let catalog = catalog_from(json!({"fetched_at": 123, "providers": providers()})).await;
    let usages = usages();
    let mut actual = String::new();
    for (provider, api, model, owner) in lookups() {
        writeln!(actual, "{provider:?} {api:?} {model:?} {owner:?}").unwrap();
        match catalog
            .metadata_with_discovery(provider, api, model, owner)
            .await
        {
            Some((metadata, discovery)) => {
                writeln!(actual, "  {metadata:?}").unwrap();
                writeln!(actual, "  {}", serde_json::to_string(&discovery).unwrap()).unwrap();
            }
            None => writeln!(actual, "  no metadata").unwrap(),
        }
        for source in [
            CatalogPricingSource::Manufacturer,
            CatalogPricingSource::Provider,
        ] {
            match catalog.lookup(provider, api, model, source).await {
                Some((cost, source_id, fetched_at)) => {
                    let rates = usages
                        .iter()
                        .map(|usage| describe_rates(parse_rates(&cost, usage)))
                        .collect::<Vec<_>>();
                    writeln!(
                        actual,
                        "  {source:?} {source_id} {fetched_at} {}",
                        rates.join(" | ")
                    )
                    .unwrap();
                }
                None => writeln!(actual, "  {source:?} no cost").unwrap(),
            }
        }
    }
    assert_eq!(actual, include_str!("fixtures/equivalence.txt"));
}

fn describe_rates(rates: Option<TokenRates>) -> String {
    let Some(rates) = rates else {
        return "none".into();
    };
    [
        Some(rates.input),
        Some(rates.output),
        rates.cache_read,
        rates.cache_write,
        rates.reasoning,
    ]
    .map(|rate| rate.map_or_else(|| "-".into(), |rate| rate.0.to_string()))
    .join("/")
}
