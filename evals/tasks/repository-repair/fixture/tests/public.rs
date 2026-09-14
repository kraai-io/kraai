use repository_repair::{Mount, Overrides, Resolved, RouteError, Router, Settings};

fn router() -> Router {
    Router::new(
        Settings {
            root: "/srv/site".into(),
            index: "index.html".into(),
            cache_seconds: 60,
        },
        vec![Mount {
            prefix: "/docs".into(),
            settings: Overrides {
                root: Some("/srv/manual".into()),
                ..Overrides::default()
            },
        }],
    )
}

#[test]
fn routes_existing_files_and_directory_indexes() {
    assert_eq!(
        router().resolve("/docs/guide.txt?download=1", &Overrides::default()),
        Ok(Resolved {
            mount: Some("/docs".into()),
            asset: "/srv/manual/guide.txt".into(),
            cache_seconds: 60,
            query: Some("download=1".into()),
        })
    );
    assert_eq!(
        router().resolve("/", &Overrides::default()).unwrap().asset,
        "/srv/site/index.html"
    );
    assert_eq!(
        router()
            .resolve("/docs/", &Overrides::default())
            .unwrap()
            .asset,
        "/srv/manual/index.html"
    );
}

#[test]
fn decodes_filenames_and_rejects_malformed_targets() {
    assert_eq!(
        router()
            .resolve("/hello%20world.txt", &Overrides::default())
            .unwrap()
            .asset,
        "/srv/site/hello world.txt"
    );
    assert_eq!(
        router().resolve("relative", &Overrides::default()),
        Err(RouteError::InvalidTarget)
    );
    assert_eq!(
        router().resolve("/%Q0", &Overrides::default()),
        Err(RouteError::InvalidEscape)
    );
    assert_eq!(
        router().resolve("/a/../secret", &Overrides::default()),
        Err(RouteError::UnsafePath)
    );
}
