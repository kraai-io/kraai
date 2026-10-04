use super::*;
use color_eyre::eyre::Result;
use kraai_types::{ConversationItem, RequestCost, Usd};

fn prefix(turn: usize) -> Result<Prefix> {
    let messages: Vec<_> = (0..=turn)
        .map(|index| ConversationItem::User {
            content: (format!("history {index}")).into(),
        })
        .collect();
    Prefix::new(&messages, &None)
}

fn usage(total: usize, cached: usize) -> TokenUsage {
    TokenUsage {
        input_tokens: total - cached,
        cache_read_tokens: cached,
        output_tokens: 60,
        reasoning_tokens: 10,
        cost: Some(RequestCost {
            amount: Usd(0),
            source: "fixture".into(),
            priced_at: 0,
            upstream: None,
            rates: Some(TokenRates {
                input: Usd(10),
                output: Usd(50),
                cache_read: Some(Usd(1)),
                cache_write: None,
                reasoning: None,
            }),
        }),
        ..Default::default()
    }
}

fn feedback(predicted: usize, growth: usize) -> Result<Feedback> {
    let mut feedback = Feedback::default();
    let initial = predicted - 6 * growth;
    let warmup = CompletedWarmup {
        prefix: prefix(0)?,
        input_tokens: initial,
    };
    feedback.observe_warmup(&usage(initial, 0));
    for i in 0..=5 {
        feedback.observe(
            prefix(i)?,
            vec![42],
            &usage(initial + i * growth + 5436, initial / 2),
            Some(&warmup),
        );
    }
    Ok(feedback)
}

#[test]
fn interval_balances_history_growth_and_measured_generation_cost() -> Result<()> {
    let policy = CacheWarmingPolicy::default();
    for (history, expected) in [(10000, 2), (40000, 3), (90000, 5)] {
        let feedback = feedback(history, 900)?;
        assert_eq!(feedback.predicted_prefix(&prefix(6)?), Some(history as f64));
        assert_eq!(feedback.interval(policy, &prefix(6)?), expected);
    }
    Ok(())
}

#[test]
fn output_and_reasoning_prices_affect_the_interval() -> Result<()> {
    let mut feedback = feedback(10000, 900)?;
    assert_eq!(
        feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?),
        2
    );
    feedback.observe_warmup(&TokenUsage {
        reasoning_tokens: 10000,
        ..usage(10000, 5000)
    });
    assert_eq!(
        feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?),
        8
    );
    Ok(())
}

#[test]
fn no_discount_is_detected_and_missing_prices_use_the_fallback_interval() -> Result<()> {
    let mut feedback = feedback(10000, 900)?;
    feedback.rates = None;
    assert_eq!(
        feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?),
        5
    );
    let mut expensive = usage(12000, 0);
    if let Some(rates) = expensive.cost.as_mut().and_then(|cost| cost.rates.as_mut()) {
        rates.cache_read = Some(Usd(10));
    }
    feedback.observe_warmup(&expensive);
    assert!(feedback.has_no_discount());
    Ok(())
}

#[test]
fn changed_pinned_files_preserve_estimates_but_rewritten_history_resets_them() -> Result<()> {
    let mut feedback = feedback(40000, 900)?;
    feedback.observe(prefix(6)?, vec![99], &usage(100000, 20000), None);
    assert_eq!(feedback.predicted_prefix(&prefix(6)?), Some(40000.0));
    assert_eq!(feedback.mean_growth(), Some(900.0));
    feedback.observe(prefix(7)?, vec![99], &usage(100500, 20000), None);
    assert_eq!(feedback.predicted_prefix(&prefix(7)?), Some(40500.0));
    let changed = Prefix::new(
        &[ConversationItem::User {
            content: "compacted".into(),
        }],
        &None,
    )?;
    let mut feedback = super::tests::feedback(40000, 900)?;
    feedback.synchronize(&changed);
    assert!(feedback.growth.is_empty());
    assert!(feedback.predicted_prefix(&changed).is_none());
    Ok(())
}

#[test]
fn repeated_usage_and_output_tokens_do_not_inflate_input_growth() -> Result<()> {
    let mut feedback = feedback(40000, 900)?;
    let mut repeated = usage(40000 - 900 + 5436, 10000);
    repeated.output_tokens = 100000;
    repeated.reasoning_tokens = 100000;
    feedback.observe(prefix(5)?, vec![42], &repeated, None);
    assert_eq!(feedback.mean_growth(), Some(900.0));
    assert_eq!(feedback.predicted_prefix(&prefix(6)?), Some(40000.0));
    assert_eq!(
        input_tokens(&TokenUsage {
            input_tokens: 100,
            cache_read_tokens: 200,
            cache_write_tokens: 300,
            output_tokens: 400,
            ..Default::default()
        }),
        600
    );
    Ok(())
}

#[test]
fn recent_growth_replaces_older_samples_and_flat_history_does_not_warm_more_often() -> Result<()> {
    let mut feedback = feedback(40000, 900)?;
    for i in 6..=10 {
        feedback.observe(prefix(i)?, vec![42], &usage(39100 + 5436, 10000), None);
    }
    assert_eq!(feedback.mean_growth(), Some(0.0));
    assert_eq!(
        feedback.interval(CacheWarmingPolicy::default(), &prefix(11)?),
        5
    );
    assert_eq!(feedback.predicted_prefix(&prefix(11)?), Some(39100.0));
    Ok(())
}

#[test]
fn intervals_cover_different_reasoning_costs_without_a_five_request_cap() -> Result<()> {
    for (reasoning, expected) in [(95, 5), (1000, 6), (4000, 8), (10000, 12)] {
        let mut feedback = feedback(80000, 1000)?;
        feedback.output.clear();
        feedback.observe_warmup(&TokenUsage {
            output_tokens: 363,
            reasoning_tokens: reasoning,
            ..usage(80000, 60000)
        });
        assert_eq!(
            feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?),
            expected
        );
    }
    Ok(())
}

#[test]
fn separate_reasoning_price_changes_interval_and_payback() -> Result<()> {
    let mut feedback = feedback(80000, 1000)?;
    feedback.output.clear();
    let mut warmup = usage(80000, 60000);
    warmup.output_tokens = 363;
    warmup.reasoning_tokens = 1000;
    let rates = warmup
        .cost
        .as_mut()
        .and_then(|cost| cost.rates.as_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("missing rates"))?;
    rates.reasoning = Some(Usd(500));
    feedback.observe_warmup(&warmup);
    assert_eq!(
        feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?),
        12
    );
    let payback = feedback
        .payback(CacheWarmingPolicy::default(), &prefix(6)?, false)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing payback"))?;
    assert_eq!(
        payback.warmup_cost,
        80000.0 + 43000.0 * 9.0 + 363.0 * 50.0 + 1000.0 * 500.0
    );
    Ok(())
}

#[test]
fn payback_charges_all_input_and_generation_before_crediting_reuse() -> Result<()> {
    for (tokens, cached, output, reasoning, profitable) in [
        (79751, 78336, 1349, 0, false),
        (49620, 49152, 528, 0, false),
        (29550, 19968, 119, 20, true),
        (9588, 0, 2185, 0, true),
        (80000, 60000, 363, 10000, false),
    ] {
        let mut feedback = Feedback::default();
        let prefix = prefix(0)?;
        let warmup = CompletedWarmup {
            prefix: prefix.clone(),
            input_tokens: tokens,
        };
        feedback.observe_warmup(&TokenUsage {
            output_tokens: output,
            reasoning_tokens: reasoning,
            ..usage(tokens, cached)
        });
        feedback.observe(
            prefix.clone(),
            vec![42],
            &usage(tokens + 5000, cached),
            Some(&warmup),
        );
        let payback = feedback
            .payback(CacheWarmingPolicy::default(), &prefix, false)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing payback"))?;
        let gap = (tokens - cached) as f64;
        assert_eq!(
            payback.warmup_cost,
            tokens as f64 + gap * 9.0 + (output + reasoning) as f64 * 50.0
        );
        assert_eq!(payback.expected_saving, 5.0 * 0.85 * 9.0 * gap);
        assert_eq!(payback.expected_saving > payback.warmup_cost, profitable);
    }
    Ok(())
}

#[test]
fn repeated_measurements_update_cache_hits_and_expiration_discards_them() -> Result<()> {
    let mut feedback = feedback(40000, 900)?;
    let prefix = prefix(5)?;
    for (cached, gap) in [(39000, 100.0), (10000, 29100.0), (44000, 0.0)] {
        feedback.observe(prefix.clone(), vec![42], &usage(44536, cached), None);
        assert_eq!(feedback.uncached_tokens(&prefix, false), Some(gap));
        assert_eq!(feedback.uncached_tokens(&prefix, true), Some(39100.0));
        assert_eq!(feedback.mean_growth(), Some(900.0));
    }
    assert_eq!(
        feedback.uncached_tokens(&super::tests::prefix(6)?, false),
        Some(900.0)
    );
    Ok(())
}

#[test]
fn expensive_generation_samples_age_out() -> Result<()> {
    let mut feedback = feedback(80000, 1000)?;
    feedback.output.clear();
    feedback.observe_warmup(&TokenUsage {
        reasoning_tokens: 10000,
        ..usage(80000, 60000)
    });
    assert!(feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?) > 5);
    for _ in 0..SAMPLE_COUNT {
        feedback.observe_warmup(&usage(80000, 60000));
    }
    assert_eq!(
        feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?),
        4
    );
    Ok(())
}
