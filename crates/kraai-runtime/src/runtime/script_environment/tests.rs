use super::*;

#[test]
fn path_replacement_respects_platform_case_sensitivity() {
    let mut environment = BTreeMap::from([
        (String::from("Path"), String::from("inherited")),
        (String::from("paTH"), String::from("mixed")),
        (String::from("PATH"), String::from("old")),
        (String::from("KRAAI_TEST"), String::from("preserved")),
    ]);

    set_path(&mut environment, String::from("packaged"));

    let mut expected = BTreeMap::from([
        (String::from("PATH"), String::from("packaged")),
        (String::from("KRAAI_TEST"), String::from("preserved")),
    ]);
    if !cfg!(windows) {
        expected.insert(String::from("Path"), String::from("inherited"));
        expected.insert(String::from("paTH"), String::from("mixed"));
    }
    assert_eq!(environment, expected);
}
