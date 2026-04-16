// Compile-fail tests using trybuild
// These tests verify that the derive macro produces proper error messages
// when given invalid input

#[test]
fn compile_fail_tests() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
