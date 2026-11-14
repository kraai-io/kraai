use kraai_runtime::AgentProfileSource;

use super::super::super::AppState;
use super::super::sheet::{Note, Sheet, SheetItem, SheetRow, Tone};
use super::{notice, plural};

pub(super) fn agents(state: &AppState) -> Sheet {
    let agents = state.filtered_agents();
    let highlighted = agents.get(state.sheet_index.min(agents.len().saturating_sub(1)));
    let mut notes = Vec::new();
    if let Some(profile) = highlighted {
        notes.push(Note::default());
        if profile.display_name != profile.id {
            notes.push(Note::field("id", &profile.id));
        }
        notes.push(Note::field(
            "sandbox",
            profile
                .capabilities
                .iter()
                .map(kraai_execution_types::SandboxCapability::as_str)
                .collect::<Vec<_>>()
                .join("  "),
        ));
        notes.push(Note::field(
            "escalation",
            profile.escalation_policy.as_str(),
        ));
        notes.push(Note::field(
            "source",
            match profile.source {
                AgentProfileSource::BuiltIn => "built-in",
                AgentProfileSource::Global => "global",
                AgentProfileSource::Workspace => "workspace",
            },
        ));
    }
    notes.extend(
        state
            .agent_profile_warnings
            .iter()
            .map(|warning| Note::new(&warning.message, Tone::Waiting)),
    );
    notes.extend(notice(state));
    Sheet {
        caption: String::from("agent"),
        count: plural(agents.len(), "agent"),
        rows: agents
            .iter()
            .map(|profile| {
                SheetRow::Item(SheetItem {
                    label: profile.display_name.clone(),
                    detail: profile.description.clone(),
                    tag: if state.selected_profile_id.as_ref() == Some(&profile.id) {
                        String::from("current")
                    } else {
                        String::new()
                    },
                    ..SheetItem::default()
                })
            })
            .collect(),
        selected: state.sheet_index,
        empty: if state.agent_profiles.is_empty() {
            String::from("No agents available")
        } else {
            String::from("No agents match")
        },
        notes,
        align_choices: true,
    }
}
