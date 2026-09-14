use crate::Overrides;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub prefix: String,
    pub settings: Overrides,
}

pub(crate) fn select<'a>(mounts: &'a [Mount], path: &str) -> Option<&'a Mount> {
    let mut best: Option<&Mount> = None;
    for mount in mounts {
        if path.starts_with(&mount.prefix)
            && best.is_none_or(|current| mount.prefix.len() > current.prefix.len())
        {
            best = Some(mount);
        }
    }
    best
}
