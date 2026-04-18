use super::*;

impl AgentManager {
    pub async fn parse_tool_calls_from_content(
        &mut self,
        session_id: &str,
        source_message_id: &MessageId,
        content: &str,
    ) -> Result<(Vec<DetectedToolCall>, Vec<ParseFailure>)> {
        let session = self.require_session(session_id).await?;
        let (
            active_tool_config,
            active_turn_profile,
            active_turn_auto_approve,
            active_turn_tool_state_snapshot,
            mut next_queue_order,
        ) = {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            let Some(active_turn_profile) = state.active_turn_profile.clone() else {
                return Ok((Vec::new(), Vec::new()));
            };
            let active_turn_tool_state_snapshot = state
                .active_turn_tool_state_snapshot
                .clone()
                .unwrap_or_default();
            (
                state.active_tool_config.clone(),
                active_turn_profile,
                state.active_turn_auto_approve,
                active_turn_tool_state_snapshot,
                state.next_tool_queue_order,
            )
        };

        let result = toon_parser::parse_tool_calls(content);

        let mut detected_calls = Vec::new();
        let mut pending_tool_calls = Vec::new();
        let mut failed_calls = result.failed;
        let allowed_tools = active_turn_profile
            .tools
            .iter()
            .map(|tool_id| tool_id.as_str())
            .collect::<HashSet<&str>>();

        for parsed in result.successful {
            let call_id = CallId::new(Ulid::new());
            let tool_id = ToolId::new(&parsed.tool_id);
            if !allowed_tools.contains(parsed.tool_id.as_str()) {
                failed_calls.push(ParseFailure {
                    kind: ParseFailureKind::ToolCall,
                    raw_content: parsed.raw_content,
                    error: format!(
                        "Tool '{}' is not allowed by the active profile '{}'",
                        parsed.tool_id, active_turn_profile.id
                    ),
                });
                continue;
            }
            let prepared = match self.tools.prepare_tool(&tool_id, parsed.args.clone()) {
                Ok(prepared) => prepared,
                Err(error) => {
                    failed_calls.push(ParseFailure {
                        kind: ParseFailureKind::ToolCall,
                        raw_content: parsed.raw_content,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let assessment = prepared.assess(&kraai_tool_core::ToolContext {
                global_config: &active_tool_config,
                tool_state_snapshot: &active_turn_tool_state_snapshot,
            });
            let description = prepared.describe();
            let requires_confirmation = !active_turn_auto_approve
                && !assessment.is_auto_approved(active_turn_profile.default_risk_level);

            let call = ToolCall {
                call_id: call_id.clone(),
                tool_id,
                args: parsed.args,
            };

            pending_tool_calls.push((
                call_id.clone(),
                PendingToolCall {
                    call,
                    source_message_id: source_message_id.clone(),
                    prepared,
                    description: description.clone(),
                    assessment: assessment.clone(),
                    config: active_tool_config.clone(),
                    tool_state_snapshot: active_turn_tool_state_snapshot.clone(),
                    status: if requires_confirmation {
                        PermissionStatus::Pending
                    } else {
                        PermissionStatus::Approved
                    },
                    queue_order: next_queue_order,
                },
            ));

            detected_calls.push(DetectedToolCall {
                call_id,
                tool_id: parsed.tool_id,
                source_message_id: source_message_id.clone(),
                description,
                assessment,
                requires_confirmation,
                queue_order: next_queue_order,
            });
            next_queue_order = next_queue_order.saturating_add(1);
        }

        let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
        state.pending_tool_calls.extend(pending_tool_calls);
        state.next_tool_queue_order = next_queue_order;

        Ok((detected_calls, failed_calls))
    }

    pub async fn add_parse_failures_to_history(
        &mut self,
        session_id: &str,
        failed: Vec<ParseFailure>,
    ) -> Result<()> {
        let agent_profile_id = self.current_turn_profile_id(session_id);
        for failure in failed {
            let content = match failure.kind {
                ParseFailureKind::ToolCall => format!(
                    "Failed to parse tool call:\n```\n{}\n```\nError: {}",
                    failure.raw_content, failure.error
                ),
                ParseFailureKind::ThinkingBlock => format!(
                    "Failed to parse thinking block:\n```\n{}\n```\nError: {}",
                    failure.raw_content, failure.error
                ),
            };
            self.add_message_with_tool_state(
                session_id,
                ChatRole::Tool,
                content,
                agent_profile_id.clone(),
                None,
                Vec::new(),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn process_message_for_tools(
        &mut self,
        session_id: &str,
        message_id: &MessageId,
    ) -> Result<Vec<DetectedToolCall>> {
        let history = self.get_chat_history(session_id).await?;
        let message = match history.get(message_id) {
            Some(message) => message,
            None => return Ok(vec![]),
        };

        let (detected, failed) = self
            .parse_tool_calls_from_content(session_id, message_id, &message.content)
            .await?;

        if !failed.is_empty() {
            self.add_parse_failures_to_history(session_id, failed)
                .await?;
        }

        Ok(detected)
    }

    pub fn approve_tool(&mut self, session_id: &str, call_id: CallId) -> bool {
        self.session_states
            .get_mut(session_id)
            .and_then(|state| state.pending_tool_calls.get_mut(&call_id))
            .map(|pending| {
                pending.status = PermissionStatus::Approved;
            })
            .is_some()
    }

    pub fn deny_tool(&mut self, session_id: &str, call_id: CallId) -> bool {
        self.session_states
            .get_mut(session_id)
            .and_then(|state| state.pending_tool_calls.get_mut(&call_id))
            .map(|pending| {
                pending.status = PermissionStatus::Denied;
            })
            .is_some()
    }

    pub fn list_pending_tools(&self, session_id: &str) -> Vec<PendingToolInfo> {
        let Some(state) = self.session_states.get(session_id) else {
            return Vec::new();
        };

        let mut tools: Vec<_> = state
            .pending_tool_calls
            .values()
            .map(|pending| PendingToolInfo {
                call_id: pending.call.call_id.clone(),
                tool_id: pending.call.tool_id.clone(),
                args: pending.call.args.clone(),
                description: pending.description.clone(),
                risk_level: pending.assessment.risk,
                reasons: pending.assessment.reasons.clone(),
                approved: match pending.status {
                    PermissionStatus::Pending => None,
                    PermissionStatus::Approved => Some(true),
                    PermissionStatus::Denied => Some(false),
                },
                queue_order: pending.queue_order,
            })
            .collect();
        tools.sort_by_key(|tool| tool.queue_order);
        tools
    }

    pub fn session_waiting_for_approval(&self, session_id: &str) -> bool {
        self.session_states.get(session_id).is_some_and(|state| {
            state
                .pending_tool_calls
                .values()
                .any(|pending| pending.status == PermissionStatus::Pending)
        })
    }

    pub fn clear_active_turn(&mut self, session_id: &str) {
        if let Some(state) = self.session_states.get_mut(session_id) {
            state.active_turn_profile = None;
            state.active_turn_auto_approve = false;
            state.active_turn_tool_state_snapshot = None;
        }
    }

    pub fn is_turn_active(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| state.active_turn_profile.is_some())
    }

    pub async fn streaming_session_ids(&self) -> HashSet<String> {
        self.streaming_messages
            .read()
            .await
            .values()
            .map(|state| state.session_id.clone())
            .collect()
    }

    pub fn cloned_tool_manager(&self) -> ToolManager {
        self.tools.clone()
    }

    pub fn cloned_provider_manager(&self) -> ProviderManager {
        self.providers.clone()
    }

    pub fn take_ready_tool_executions(&mut self, session_id: &str) -> Vec<ToolExecutionRequest> {
        let Some(state) = self.session_states.get_mut(session_id) else {
            return Vec::new();
        };

        let ready_ids: Vec<_> = state
            .pending_tool_calls
            .iter()
            .filter(|(_, pending)| pending.status != PermissionStatus::Pending)
            .map(|(call_id, _)| call_id.clone())
            .collect();

        let mut executions = Vec::new();
        for call_id in ready_ids {
            let Some(pending) = state.pending_tool_calls.remove(&call_id) else {
                continue;
            };
            *state
                .in_flight_tool_calls
                .entry(pending.source_message_id.clone())
                .or_default() += 1;

            match pending.status {
                PermissionStatus::Pending => {}
                PermissionStatus::Approved => executions.push(ToolExecutionRequest {
                    call_id,
                    tool_id: pending.call.tool_id,
                    source_message_id: pending.source_message_id,
                    payload: ToolExecutionPayload::Approved {
                        prepared: pending.prepared,
                        config: pending.config,
                        tool_state_snapshot: pending.tool_state_snapshot,
                    },
                }),
                PermissionStatus::Denied => executions.push(ToolExecutionRequest {
                    call_id,
                    tool_id: pending.call.tool_id,
                    source_message_id: pending.source_message_id,
                    payload: ToolExecutionPayload::Denied,
                }),
            }
        }

        executions
    }

    pub fn finish_tool_executions(&mut self, session_id: &str, source_message_ids: &[MessageId]) {
        let Some(state) = self.session_states.get_mut(session_id) else {
            return;
        };

        for source_message_id in source_message_ids {
            let mut should_remove = false;
            if let Some(in_flight) = state.in_flight_tool_calls.get_mut(source_message_id) {
                if *in_flight > 0 {
                    *in_flight -= 1;
                }
                should_remove = *in_flight == 0;
            }
            if should_remove {
                state.in_flight_tool_calls.remove(source_message_id);
            }
        }
    }

    pub async fn add_tool_results_to_history(
        &mut self,
        session_id: &str,
        results: Vec<ToolResult>,
    ) -> Result<()> {
        let agent_profile_id = self.current_turn_profile_id(session_id);
        for result in results {
            let content = kraai_types::format_tool_result_message(
                &result.tool_id,
                &result.output,
                result.permission_denied,
            );

            tracing::debug!(
                "Adding tool result to history: session={}, tool_id={}, denied={}",
                session_id,
                result.tool_id,
                result.permission_denied
            );

            self.add_message_with_tool_state(
                session_id,
                ChatRole::Tool,
                content,
                agent_profile_id.clone(),
                None,
                result.tool_state_deltas,
            )
            .await?;
        }
        Ok(())
    }

    pub fn has_pending_tools(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| !state.pending_tool_calls.is_empty())
    }

    pub fn has_unfinished_tools_for_message(
        &self,
        session_id: &str,
        source_message_id: &MessageId,
    ) -> bool {
        self.session_states.get(session_id).is_some_and(|state| {
            state
                .pending_tool_calls
                .values()
                .any(|pending| &pending.source_message_id == source_message_id)
                || state
                    .in_flight_tool_calls
                    .get(source_message_id)
                    .is_some_and(|in_flight| *in_flight > 0)
        })
    }

    pub fn get_pending_tool_args(
        &self,
        session_id: &str,
        call_id: &CallId,
    ) -> Option<serde_json::Value> {
        self.session_states
            .get(session_id)
            .and_then(|state| state.pending_tool_calls.get(call_id))
            .map(|pending| pending.call.args.clone())
    }

    pub fn get_pending_tool_assessment(
        &self,
        session_id: &str,
        call_id: &CallId,
    ) -> Option<ToolCallAssessment> {
        self.session_states
            .get(session_id)
            .and_then(|state| state.pending_tool_calls.get(call_id))
            .map(|pending| pending.assessment.clone())
    }
}
