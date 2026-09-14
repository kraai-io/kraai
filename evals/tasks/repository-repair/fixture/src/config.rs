#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub root: String,
    pub index: String,
    pub cache_seconds: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub root: Option<String>,
    pub index: Option<String>,
    pub cache_seconds: Option<u32>,
}

pub(crate) fn resolve(defaults: &Settings, mount: &Overrides, request: &Overrides) -> Settings {
    let selected = if request == &Overrides::default() {
        mount
    } else {
        request
    };
    Settings {
        root: selected
            .root
            .clone()
            .unwrap_or_else(|| defaults.root.clone()),
        index: selected
            .index
            .clone()
            .unwrap_or_else(|| defaults.index.clone()),
        cache_seconds: selected.cache_seconds.unwrap_or(defaults.cache_seconds),
    }
}
