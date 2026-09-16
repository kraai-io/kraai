use crate::{Runtime, wire};
use kraai_runtime::*;
use kraai_types::MessageId;
use napi_derive::napi;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

macro_rules! runtime_methods {
    ($($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $out:ty;)*) => {
        #[napi]
        impl Runtime {
            $(#[napi(skip_typescript)]
            pub async fn $name(&self, $($arg: Value),*) -> napi::Result<Value> {
                $(let $arg: $ty = match wire::decode($arg) {
                    Ok(value) => value,
                    Err(error) => return wire::encode::<$out>(Err(error)),
                };)*
                let result: RuntimeResult<$out> = self.handle.$name($($arg),*).await;
                wire::encode(result)
            })*
        }

        pub(crate) fn declarations(cfg: &ts_rs::Config) -> Result<String, Box<dyn std::error::Error>> {
            use ts_rs::TS;
            let mut output = String::new();
            $(
                export_dependencies::<$out>(cfg)?;
                let args: Vec<String> = vec![$({
                    export_dependencies::<$ty>(cfg)?;
                    format!("{}: {}", stringify!($arg), <$ty>::name(cfg))
                }),*];
                output.push_str(&format!("  {}({}): Promise<RuntimeResult<{}>>;\n",
                    camel_case(stringify!($name)), args.join(", "), <$out>::name(cfg)));
            )*
            Ok(output)
        }
    };
}

fn export_dependencies<T: ts_rs::TS + ?Sized + 'static>(
    cfg: &ts_rs::Config,
) -> Result<(), ts_rs::ExportError> {
    if T::output_path().is_some() {
        return T::export_all(cfg);
    }
    struct Visitor<'a> {
        cfg: &'a ts_rs::Config,
        result: Result<(), ts_rs::ExportError>,
    }
    impl ts_rs::TypeVisitor for Visitor<'_> {
        fn visit<T: ts_rs::TS + ?Sized + 'static>(&mut self) {
            if self.result.is_ok() {
                self.result = export_dependencies::<T>(self.cfg);
            }
        }
    }
    let mut visitor = Visitor {
        cfg,
        result: Ok(()),
    };
    T::visit_dependencies(&mut visitor);
    T::visit_generics(&mut visitor);
    visitor.result
}

fn camel_case(name: &str) -> String {
    let mut uppercase = false;
    name.chars()
        .filter_map(|character| {
            if character == '_' {
                uppercase = true;
                None
            } else if uppercase {
                uppercase = false;
                Some(character.to_ascii_uppercase())
            } else {
                Some(character)
            }
        })
        .collect()
}

runtime_methods! {
    wait_for_startup() -> RuntimeStartupState;
    list_models() -> HashMap<String, Vec<Model>>;
    list_provider_definitions() -> Vec<ProviderDefinition>;
    get_settings() -> SettingsDocument;
    list_agent_profiles(session_id: String) -> AgentProfilesState;
    get_agent_profile_catalog(workspace_dir: Option<String>) -> AgentProfileCatalog;
    set_session_profile(session_id: String, profile_id: String) -> ();
    save_settings(settings: SettingsDocument) -> ();
    create_session() -> String;
    create_session_with(request: CreateSessionRequest) -> String;
    send_message(session_id: String, message: String, model_id: String, provider_id: String) -> SubmitMessageOutcome;
    get_chat_history(session_id: String) -> BTreeMap<MessageId, kraai_types::Message>;
    get_session_snapshot(session_id: String) -> SessionSnapshot;
    get_session_context_usage(session_id: String) -> Option<SessionContextUsage>;
    load_session(session_id: String) -> bool;
    list_sessions() -> Vec<Session>;
    list_user_input_history(limit: usize) -> Vec<String>;
    delete_session(session_id: String) -> ();
    get_workspace_state(session_id: String) -> Option<WorkspaceState>;
    set_workspace_dir(session_id: String, workspace_dir: String) -> ();
    get_tip(session_id: String) -> Option<String>;
    undo_last_user_message(session_id: String) -> Option<String>;
    get_pending_script(session_id: String) -> Option<PendingScriptInfo>;
    approve_script(session_id: String, execution_id: String) -> ();
    deny_script(session_id: String, execution_id: String) -> ();
    cancel_stream(session_id: String) -> bool;
    continue_session(session_id: String) -> ContinueSessionOutcome;
    get_openai_codex_auth_status() -> OpenAiCodexAuthStatus;
    start_openai_codex_browser_login() -> ();
    start_openai_codex_device_code_login() -> ();
    cancel_openai_codex_login() -> ();
    logout_openai_codex_auth() -> ();
}
