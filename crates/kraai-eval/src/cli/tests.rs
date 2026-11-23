use super::*;

#[test]
fn prices_need_input_and_output() {
    let run = |extra: &[&str]| {
        let mut args = vec!["kraai-eval", "run", "in-house", "--model", "test"];
        args.extend_from_slice(extra);
        Cli::try_parse_from(args)
    };
    let Command::Run(args) = run(&[
        "--price-input",
        "1.25",
        "--price-output",
        "10",
        "--price-cache-read",
        "0.125",
    ])
    .unwrap()
    .command
    else {
        panic!("expected the run command");
    };
    let rates = args
        .pricing
        .price()
        .expect("missing price")
        .rates()
        .unwrap();
    assert!(rates.input == kraai_types::Usd(1_250_000_000));
    assert!(rates.cache_read == Some(kraai_types::Usd(125_000_000)));
    assert!(rates.reasoning.is_none());
    assert!(run(&["--price-input", "1"]).is_err());
    assert!(run(&["--price-output", "1"]).is_err());
}
