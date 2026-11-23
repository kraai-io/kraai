use super::expand_argument;

#[test]
fn runner_templates_preserve_placeholder_text_inside_prompts() {
    assert_eq!(
        expand_argument(
            "prefix {prompt} {provider_id} {unknown}",
            &[
                ("{prompt}", "Keep {provider_id} literal, including 🦀."),
                ("{provider_id}", "selected")
            ],
        ),
        "prefix Keep {provider_id} literal, including 🦀. selected {unknown}"
    );
}
