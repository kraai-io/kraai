use color_eyre::eyre::{Result, eyre};

use crate::benchmark::Workload;

crate::benchmark::register_workloads!(agent, streaming, persistence, nushell);

pub(crate) fn find(name: &str) -> Result<Workload> {
    catalog()?
        .into_iter()
        .find(|workload| workload.name == name)
        .ok_or_else(|| eyre!("Unknown performance workload: {name}"))
}
