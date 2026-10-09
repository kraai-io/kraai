use serde::{Deserialize, Serialize};

use crate::{ModelId, ProviderId};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    #[serde(default)]
    pub options: crate::ModelOptionValues,
}
