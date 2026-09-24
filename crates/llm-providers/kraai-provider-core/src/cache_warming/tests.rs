use super::*;
use crate::{ScriptToolDefinition, test_support::MockProvider};
use kraai_types::ConversationItem;

fn manager() -> ProviderManager {
    let mut manager = ProviderManager::new();
    let mut provider = MockProvider::new("mock");
    provider.warming = Some(CacheWarmingPolicy::default());
    manager.register_provider(ProviderId::new("mock"), Box::new(provider));
    manager
}

fn request() -> ProviderRequest {
    ProviderRequest {
        messages: vec![
            ConversationItem::System {
                text: "instructions".into(),
            },
            ConversationItem::User {
                text: "history ".repeat(1500),
            },
            ConversationItem::System {
                text: "pinned files".into(),
            },
        ],
        script_tool: Some(ScriptToolDefinition {
            name: "script".into(),
            description: "Execute scripts".into(),
        }),
        cacheable_messages: Some(2),
    }
}

fn prepare(manager: &ProviderManager, request: &ProviderRequest) -> Result<Option<CacheWarmup>> {
    manager.prepare_cache_warmup(
        &ProviderId::new("mock"),
        &ModelId::new("model"),
        "session",
        request,
    )
}

#[test]
fn warming_is_opt_in_and_requires_a_large_prefix_with_a_suffix() -> Result<()> {
    let mut disabled = ProviderManager::new();
    disabled.register_provider(ProviderId::new("mock"), Box::new(MockProvider::new("mock")));
    assert!(prepare(&disabled, &request())?.is_none());
    let manager = manager();
    let mut request = request();
    request.cacheable_messages = None;
    assert!(prepare(&manager, &request)?.is_none());
    request.cacheable_messages = Some(request.messages.len());
    assert!(prepare(&manager, &request).is_err());
    request.cacheable_messages = Some(1);
    assert!(prepare(&manager, &request)?.is_none());
    Ok(())
}

#[test]
fn warming_preserves_tools_and_prefix_but_excludes_the_suffix() -> Result<()> {
    let manager = manager();
    let request = request();
    let warmup = prepare(&manager, &request)?.ok_or_else(|| eyre!("missing warmup"))?;
    assert_eq!(warmup.request.script_tool, request.script_tool);
    assert_eq!(
        warmup.request.messages,
        request
            .messages
            .get(..2)
            .ok_or_else(|| eyre!("missing prefix"))?
    );
    assert_eq!(warmup.request.cacheable_messages, None);
    warmup.complete(&TokenUsage {
        input_tokens: 3000,
        ..Default::default()
    })?;
    Ok(())
}

#[test]
fn growth_requires_spacing_and_suffix_changes_do_not_rewarm() -> Result<()> {
    let manager = manager();
    let mut request = request();
    prepare(&manager, &request)?
        .ok_or_else(|| eyre!("missing warmup"))?
        .complete(&TokenUsage {
            input_tokens: 3000,
            ..Default::default()
        })?;
    request.messages.pop();
    request.messages.push(ConversationItem::System {
        text: "changed pinned files".into(),
    });
    for _ in 0..8 {
        assert!(prepare(&manager, &request)?.is_none());
    }
    request.messages.insert(
        2,
        ConversationItem::User {
            text: "new history ".repeat(1000),
        },
    );
    request.cacheable_messages = Some(3);
    prepare(&manager, &request)?
        .ok_or_else(|| eyre!("growth should warm"))?
        .complete(&TokenUsage {
            input_tokens: 3000,
            ..Default::default()
        })?;
    request.messages.insert(
        3,
        ConversationItem::User {
            text: "more history ".repeat(1000),
        },
    );
    request.cacheable_messages = Some(4);
    for _ in 0..4 {
        assert!(prepare(&manager, &request)?.is_none());
    }
    assert!(prepare(&manager, &request)?.is_some());
    Ok(())
}

#[test]
fn cancelled_and_failed_warmups_release_the_slot_without_immediate_retry() -> Result<()> {
    let manager = manager();
    let request = request();
    let warmup = prepare(&manager, &request)?.ok_or_else(|| eyre!("missing warmup"))?;
    assert!(prepare(&manager.clone(), &request)?.is_none());
    drop(warmup);
    for _ in 0..3 {
        assert!(prepare(&manager, &request)?.is_none());
    }
    assert!(prepare(&manager, &request)?.is_some());
    Ok(())
}

#[test]
fn changed_history_and_tools_invalidate_the_warmed_prefix() -> Result<()> {
    for change_tools in [false, true] {
        let manager = manager();
        let mut request = request();
        prepare(&manager, &request)?
            .ok_or_else(|| eyre!("missing warmup"))?
            .complete(&TokenUsage {
                input_tokens: 3000,
                ..Default::default()
            })?;
        if change_tools {
            request.script_tool = None;
        } else {
            request.messages.remove(1);
            request.messages.insert(
                1,
                ConversationItem::User {
                    text: "compacted or edited history ".repeat(400),
                },
            );
        }
        for _ in 0..4 {
            assert!(prepare(&manager, &request)?.is_none());
        }
        assert!(prepare(&manager, &request)?.is_some());
    }
    Ok(())
}

#[test]
fn sessions_and_models_do_not_share_warming_state() -> Result<()> {
    let manager = manager();
    let request = request();
    prepare(&manager, &request)?
        .ok_or_else(|| eyre!("missing warmup"))?
        .complete(&TokenUsage {
            input_tokens: 3000,
            ..Default::default()
        })?;
    for (model, session) in [("other-model", "session"), ("model", "other-session")] {
        assert!(
            manager
                .prepare_cache_warmup(
                    &ProviderId::new("mock"),
                    &ModelId::new(model),
                    session,
                    &request
                )?
                .is_some()
        );
    }
    Ok(())
}

#[test]
fn expired_prefix_is_refreshed_without_history_growth() -> Result<()> {
    let mut manager = manager();
    let mut provider = MockProvider::new("mock");
    provider.warming = Some(CacheWarmingPolicy {
        refresh_after: Duration::ZERO,
        ..Default::default()
    });
    manager.register_provider(ProviderId::new("mock"), Box::new(provider));
    prepare(&manager, &request())?
        .ok_or_else(|| eyre!("missing warmup"))?
        .complete(&TokenUsage {
            input_tokens: 3000,
            ..Default::default()
        })?;
    assert!(prepare(&manager, &request())?.is_some());
    Ok(())
}

fn priced_usage(input: usize) -> TokenUsage {
    TokenUsage {
        input_tokens: input / 2,
        cache_read_tokens: input - input / 2,
        output_tokens: 60,
        reasoning_tokens: 10,
        cost: Some(kraai_types::RequestCost {
            amount: kraai_types::Usd(0),
            source: "fixture".into(),
            priced_at: 0,
            upstream: None,
            rates: Some(kraai_types::TokenRates {
                input: kraai_types::Usd(10),
                output: kraai_types::Usd(50),
                cache_read: Some(kraai_types::Usd(1)),
                cache_write: None,
                reasoning: None,
            }),
        }),
        ..Default::default()
    }
}

fn observe(manager: &ProviderManager, request: &ProviderRequest, input: usize) -> Result<()> {
    manager
        .cache_usage_observer(
            &ProviderId::new("mock"),
            &ModelId::new("model"),
            request,
            &crate::ProviderRequestContext::with_prompt_cache_key("session".into()),
        )?
        .ok_or_else(|| eyre!("missing observer"))?
        .observe(&priced_usage(input))
}

fn append(request: &mut ProviderRequest, text: String) -> Result<()> {
    let boundary = request
        .cacheable_messages
        .ok_or_else(|| eyre!("missing boundary"))?;
    request
        .messages
        .insert(boundary, ConversationItem::User { text });
    request.cacheable_messages = Some(boundary + 1);
    Ok(())
}

#[test]
fn scheduling_adapts_to_observed_tokens_even_when_serialized_growth_is_tiny() -> Result<()> {
    for (initial, interval) in [(8000, 2), (40000, 3), (90000, 5)] {
        let manager = manager();
        let mut request = request();
        prepare(&manager, &request)?
            .ok_or_else(|| eyre!("missing initial warmup"))?
            .complete(&priced_usage(initial))?;
        observe(&manager, &request, initial + 5436)?;
        for turn in 1..=interval {
            append(&mut request, format!("turn {turn}"))?;
            let warmup = prepare(&manager, &request)?;
            assert_eq!(warmup.is_some(), turn == interval);
            observe(&manager, &request, initial + turn * 900 + 5436)?;
        }
    }
    Ok(())
}

#[test]
fn minimum_token_growth_prevents_warming_large_byte_changes() -> Result<()> {
    let manager = manager();
    let mut request = request();
    prepare(&manager, &request)?
        .ok_or_else(|| eyre!("missing warmup"))?
        .complete(&priced_usage(10000))?;
    observe(&manager, &request, 15436)?;
    for turn in 1..=10 {
        append(&mut request, "large serialized growth ".repeat(1000))?;
        assert!(prepare(&manager, &request)?.is_none());
        observe(&manager, &request, 15436 + turn * 10)?;
    }
    Ok(())
}

#[tokio::test]
async fn real_stream_usage_updates_warming_feedback() -> Result<()> {
    use futures::StreamExt;
    let mut manager = manager();
    let mut provider = MockProvider::new("mock");
    provider.warming = Some(CacheWarmingPolicy::default());
    let mut usage = priced_usage(5000);
    if let Some(rates) = usage.cost.as_mut().and_then(|cost| cost.rates.as_mut()) {
        rates.cache_read = Some(kraai_types::Usd(10));
    }
    provider.usage = Some(usage.clone());
    manager.register_provider(ProviderId::new("mock"), Box::new(provider));
    let mut stream = manager
        .generate_reply_stream(
            ProviderId::new("mock"),
            &ModelId::new("model"),
            request(),
            crate::ProviderRequestContext::with_prompt_cache_key("session".into()),
        )
        .await?;
    let mut reported = None;
    while let Some(event) = stream.next().await {
        if let crate::ProviderStreamEvent::Usage(usage) = event? {
            reported = Some(usage);
        }
    }
    assert_eq!(reported, Some(usage));
    assert!(prepare(&manager, &request())?.is_none());
    Ok(())
}

#[test]
fn active_matching_prefix_does_not_expire_but_idle_prefix_does() -> Result<()> {
    let manager = manager();
    prepare(&manager, &request())?
        .ok_or_else(|| eyre!("missing warmup"))?
        .complete(&TokenUsage {
            input_tokens: 3000,
            ..Default::default()
        })?;
    let context = crate::ProviderRequestContext::with_prompt_cache_key("session".into());
    let observer = manager
        .cache_usage_observer(
            &ProviderId::new("mock"),
            &ModelId::new("model"),
            &request(),
            &context,
        )?
        .ok_or_else(|| eyre!("missing observer"))?;
    observer
        .state
        .lock()
        .map_err(|error| eyre!("state lock: {error}"))?
        .last_attempt = Some(Instant::now() - Duration::from_secs(600));
    observer.observe(&TokenUsage {
        cache_read_tokens: 3000,
        input_tokens: 50,
        ..Default::default()
    })?;
    assert!(prepare(&manager, &request())?.is_none());
    observer
        .state
        .lock()
        .map_err(|error| eyre!("state lock: {error}"))?
        .last_prefix_use = Some(Instant::now() - Duration::from_secs(600));
    assert!(prepare(&manager, &request())?.is_some());
    Ok(())
}
