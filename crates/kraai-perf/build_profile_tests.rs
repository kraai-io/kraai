use color_eyre::eyre::{Result, ensure};
use serde_json::json;

use super::*;
use crate::report::{Metadata, Report, Sample, WorkloadReport, WorkloadSpec, compare};

fn report(profiles: &Value, name: &str) -> Result<Report> {
    let selected = select(
        profiles,
        &Path::new("target").join(name).join("build/perf-hash/out"),
        "release",
    )?;
    Report::new(
        Metadata {
            measurement_schema: "test".into(),
            host: BTreeMap::from([("cpu".into(), "test".into())]),
            build: BTreeMap::from([(
                "declared_profile_settings".into(),
                selected.settings.to_string(),
            )]),
            provenance: BTreeMap::from([("workspace_profiles".into(), profiles.to_string())]),
            warmups: 1,
        },
        vec![WorkloadReport::new(
            WorkloadSpec {
                name: "fixture".into(),
                fingerprint: "fixture-v1".into(),
                operations: 1,
                parameters: Value::Null,
            },
            vec![Sample {
                wall_ns: 1,
                user_cpu_ns: 1,
                system_cpu_ns: 0,
                children_user_cpu_ns: 0,
                children_system_cpu_ns: 0,
                peak_rss_bytes: 1,
                children_peak_rss_bytes: 0,
            }],
        )?],
    )
}

#[test]
fn release_comparison_ignores_unrelated_profiles_but_preserves_provenance() -> Result<()> {
    let baseline = json!({"release": {"lto": true, "codegen-units": 1}});
    let candidate = json!({
        "release": {"lto": true, "codegen-units": 1},
        "dev": {"opt-level": 2},
        "profiling": {"inherits": "release", "debug": 2}
    });
    let comparison = compare(
        &report(&baseline, "release")?,
        &report(&candidate, "release")?,
        None,
    )?;
    ensure!(comparison.workloads.len() == 1);
    ensure!(comparison.baseline_provenance != comparison.candidate_provenance);
    for changed in [
        json!({"release": {"lto": false, "codegen-units": 1}}),
        json!({"release": {"lto": true, "codegen-units": 16}}),
    ] {
        ensure!(
            compare(
                &report(&baseline, "release")?,
                &report(&changed, "release")?,
                None
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn custom_profile_merges_inherited_and_nested_settings() -> Result<()> {
    let profiles = json!({
        "release": {
            "lto": true,
            "debug": 0,
            "package": {"dependency": {"opt-level": 3, "codegen-units": 1}},
            "build-override": {"opt-level": 1, "debug": false}
        },
        "profiling": {
            "inherits": "release",
            "debug": 1,
            "package": {"dependency": {"opt-level": 2}},
            "build-override": {"debug": true}
        }
    });
    let selected = select(
        &profiles,
        Path::new("target/profiling/build/perf-hash/out"),
        "release",
    )?;
    ensure!(selected.name == "profiling");
    ensure!(
        selected.settings
            == json!({
                "lto": true,
                "debug": 1,
                "package": {"dependency": {"opt-level": 2, "codegen-units": 1}},
                "build-override": {"opt-level": 1, "debug": true}
            })
    );
    let mut shadowed = profiles.clone();
    *shadowed
        .pointer_mut("/release/debug")
        .ok_or_else(|| io::Error::other("missing debug"))? = json!(2);
    compare(
        &report(&profiles, "profiling")?,
        &report(&shadowed, "profiling")?,
        None,
    )?;
    *shadowed
        .pointer_mut("/release/lto")
        .ok_or_else(|| io::Error::other("missing lto"))? = json!(false);
    ensure!(
        compare(
            &report(&profiles, "profiling")?,
            &report(&shadowed, "profiling")?,
            None
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn resolves_cross_target_paths_and_preserves_unavailable_nix_settings() -> Result<()> {
    let profiles = json!({"release": {"lto": true}});
    let selected = select(
        &profiles,
        Path::new("target/aarch64-apple-darwin/release/build/perf-hash/out"),
        "release",
    )?;
    ensure!(selected.name == "release");
    ensure!(selected.settings == json!({"lto": true}));
    let selected = select(
        &Value::Null,
        Path::new("target/build/kraai-perf.out"),
        "release",
    )?;
    ensure!(selected.name == "release");
    ensure!(selected.settings.is_null());
    let selected = select(
        &json!({}),
        Path::new("target/debug/build/perf-hash/out"),
        "debug",
    )?;
    ensure!(selected.name == "dev");
    ensure!(selected.settings == json!({}));
    Ok(())
}

#[test]
fn captures_only_selected_profile_overrides_and_watches_unset_options() -> Result<()> {
    let profiles = json!({"release-fast": {"inherits": "release"}});
    let selected = select(
        &profiles,
        Path::new("target/release-fast/build/perf-hash/out"),
        "release",
    )?;
    let relevant = BTreeMap::from([
        ("CARGO_PROFILE_RELEASE_LTO".into(), "thin".into()),
        ("CARGO_PROFILE_RELEASE_FAST_DEBUG".into(), "1".into()),
        (
            "CARGO_PROFILE_RELEASE_FAST_BUILD_OVERRIDE_OPT_LEVEL".into(),
            "2".into(),
        ),
        ("CARGO_FEATURE_TEST".into(), "1".into()),
    ]);
    let mut environment = relevant.clone();
    environment.insert("CARGO_PROFILE_DEV_OPT_LEVEL".into(), "2".into());
    environment.insert("CARGO_PROFILE_RELEASE_FAST_EXTRA_DEBUG".into(), "2".into());
    ensure!(selected.overrides(&environment) == relevant);
    let release = select(
        &profiles,
        Path::new("target/release/build/perf-hash/out"),
        "release",
    )?;
    ensure!(
        !release
            .overrides(&environment)
            .contains_key("CARGO_PROFILE_RELEASE_FAST_DEBUG")
    );
    ensure!(
        selected
            .environment_keys()
            .contains(&"CARGO_PROFILE_RELEASE_FAST_CODEGEN_UNITS".into())
    );
    Ok(())
}

#[test]
fn rejects_invalid_inheritance() {
    let path = Path::new("target/custom/build/perf-hash/out");
    for profiles in [
        json!({"custom": {"inherits": "custom"}}),
        json!({"custom": {"inherits": "missing"}}),
        json!({"custom": {"inherits": 42}}),
    ] {
        assert!(select(&profiles, path, "release").is_err());
    }
}
