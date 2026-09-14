use crate::{Mount, Overrides, RouteError, Settings, config, mounts, path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub mount: Option<String>,
    pub asset: String,
    pub cache_seconds: u32,
    pub query: Option<String>,
}

pub struct Router {
    defaults: Settings,
    mounts: Vec<Mount>,
}

impl Router {
    pub fn new(defaults: Settings, mounts: Vec<Mount>) -> Self {
        Self { defaults, mounts }
    }

    pub fn resolve(&self, target: &str, request: &Overrides) -> Result<Resolved, RouteError> {
        let target = path::parse(target)?;
        let mount = mounts::select(&self.mounts, &target.path);
        let settings = config::resolve(
            &self.defaults,
            &mount
                .map(|value| value.settings.clone())
                .unwrap_or_default(),
            request,
        );
        let relative = mount
            .map_or(target.path.as_str(), |mount| {
                &target.path[mount.prefix.len()..]
            })
            .trim_start_matches('/');
        let mut asset = settings.root.trim_end_matches('/').to_owned();
        asset.push('/');
        asset.push_str(relative);
        if relative.is_empty() || target.directory {
            if !asset.ends_with('/') {
                asset.push('/');
            }
            asset.push_str(&settings.index);
        }
        Ok(Resolved {
            mount: mount.map(|mount| mount.prefix.clone()),
            asset,
            cache_seconds: settings.cache_seconds,
            query: target.query,
        })
    }
}
