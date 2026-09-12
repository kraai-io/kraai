use dependency_waves::{Job, plan};

#[test]
fn dependencies_and_capacity() {
    let jobs = vec![
        Job {
            id: "ship".into(),
            dependencies: vec!["test".into()],
            resources: vec![],
        },
        Job {
            id: "test".into(),
            dependencies: vec!["build".into()],
            resources: vec![],
        },
        Job {
            id: "build".into(),
            dependencies: vec![],
            resources: vec![],
        },
        Job {
            id: "docs".into(),
            dependencies: vec![],
            resources: vec![],
        },
    ];
    assert_eq!(
        plan(&jobs, &[], 2),
        Ok(vec![
            vec!["build".into(), "docs".into()],
            vec!["test".into()],
            vec!["ship".into()]
        ])
    );
}

#[test]
fn resource_conflicts_do_not_stop_the_scan() {
    let jobs = ["a", "b", "c"].map(|id| Job {
        id: id.into(),
        dependencies: vec![],
        resources: if id == "c" {
            vec![]
        } else {
            vec!["writer".into()]
        },
    });
    assert_eq!(
        plan(&jobs, &[], 2),
        Ok(vec![vec!["a".into(), "c".into()], vec!["b".into()]])
    );
}
