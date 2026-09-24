use super::*;
use color_eyre::eyre::Result;
use kraai_types::{ConversationItem, RequestCost, Usd};

fn prefix(turn: usize) -> Result<Prefix> {
    let messages: Vec<_> = (0..=turn)
        .map(|index| ConversationItem::User {
            text: format!("history {index}"),
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
        5
    );
    Ok(())
}

#[test]
fn no_discount_is_detected_and_missing_prices_use_the_maximum_interval() -> Result<()> {
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
fn changed_pinned_files_and_rewritten_history_reset_growth() -> Result<()> {
    let mut feedback = feedback(40000, 900)?;
    feedback.synchronize(&prefix(6)?, &[99]);
    assert!(feedback.predicted_prefix(&prefix(6)?).is_none());
    assert_eq!(
        feedback.interval(CacheWarmingPolicy::default(), &prefix(6)?),
        5
    );
    let changed = Prefix::new(
        &[ConversationItem::User {
            text: "compacted".into(),
        }],
        &None,
    )?;
    let mut feedback = super::tests::feedback(40000, 900)?;
    feedback.synchronize(&changed, &[42]);
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
