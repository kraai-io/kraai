mod events;
mod methods;
mod wire;

use kraai_runtime::{RuntimeBuilder, RuntimeHandle};
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

pub use events::EventSubscription;

#[derive(Deserialize, Serialize, TS)]
#[ts(export_to = "types.d.ts")]
pub struct RuntimeOptions {
    pub provider_config_path: Option<String>,
    pub storage_root: Option<String>,
    pub nushell_host_path: String,
    pub script_runtime_roots: Option<Vec<String>>,
}

#[napi]
pub struct Runtime {
    handle: RuntimeHandle,
    closed: CancellationToken,
}

#[napi]
impl Runtime {
    #[napi(factory, async_runtime, skip_typescript)]
    pub fn create(options: Value) -> napi::Result<Self> {
        let options: RuntimeOptions =
            wire::decode(options).map_err(|error| napi::Error::from_reason(error.to_string()))?;
        let path = std::path::PathBuf::from(options.nushell_host_path);
        if !path.is_absolute() || !path.is_file() {
            return Err(napi::Error::from_reason(
                "nushell_host_path must be an existing absolute path",
            ));
        }
        let mut builder = RuntimeBuilder::new().nushell_host_path(path);
        if let Some(roots) = options.script_runtime_roots {
            builder = builder.script_runtime_roots(roots.into_iter().map(Into::into).collect());
        }
        if let Some(path) = options.storage_root {
            builder = builder.storage_root(path.into());
        }
        if let Some(path) = options.provider_config_path {
            builder = builder.provider_config_path(path.into());
        }
        Ok(Self {
            handle: builder.build_on(&tokio::runtime::Handle::current()),
            closed: CancellationToken::new(),
        })
    }

    #[napi]
    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription::new(self.handle.subscribe(), self.closed.child_token())
    }

    #[napi(skip_typescript)]
    pub fn startup_status(&self) -> napi::Result<Value> {
        wire::value(self.handle.startup_status())
    }

    #[napi(skip_typescript)]
    pub async fn shutdown(&self) -> napi::Result<Value> {
        self.closed.cancel();
        wire::encode(self.handle.shutdown().await)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}

pub fn export_types(directory: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = ts_rs::Config::new()
        .with_large_int("number")
        .with_out_dir(directory);
    RuntimeOptions::export_all(&cfg)?;
    kraai_runtime::RuntimeError::export_all(&cfg)?;
    kraai_runtime::RuntimeStartupState::export_all(&cfg)?;
    events::EventRead::export_all(&cfg)?;
    let methods = methods::declarations(&cfg)?;
    let types = std::fs::read_to_string(directory.join("types.d.ts"))?;
    let declarations = format!(
        "{types}\nexport type RuntimeResult<T> = {{ Ok: T }} | {{ Err: RuntimeError }};\nexport declare class Runtime {{\n  private constructor();\n  static create(options: RuntimeOptions): Runtime;\n  subscribe(): EventSubscription;\n  startupStatus(): RuntimeStartupState;\n  shutdown(): Promise<RuntimeResult<null>>;\n{methods}}}\nexport declare class EventSubscription {{\n  private constructor();\n  next(): Promise<EventRead>;\n  close(): void;\n}}\n"
    );
    std::fs::write(directory.join("index.d.ts"), declarations)?;
    Ok(())
}
