use super::*;

#[test]
fn distribution_uses_nearest_rank_percentiles() {
    let summary = distribution(vec![50, 10, 30, 20, 40]);
    assert_eq!(summary.samples, 5);
    assert_eq!(summary.min, Some(10));
    assert_eq!(summary.max, Some(50));
    assert_eq!(summary.mean, Some(30.0));
    assert_eq!(summary.p50, Some(30));
    assert_eq!(summary.p95, Some(50));
}
