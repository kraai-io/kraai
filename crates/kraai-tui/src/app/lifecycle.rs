use super::*;

impl App {
    pub(super) fn update_chat_viewport(&mut self, height: u16) {
        self.state.chat_viewport_height = height;
        self.clamp_chat_scroll();
    }

    pub(super) fn effective_chat_scroll(&self) -> u16 {
        let max_scroll = self.state.chat_max_scroll();
        if self.state.auto_scroll {
            max_scroll
        } else {
            self.state.scroll.min(max_scroll)
        }
    }

    pub(super) fn clamp_chat_scroll(&mut self) {
        self.state.scroll = self.effective_chat_scroll();
    }

    pub(super) fn scroll_chat_by(&mut self, delta: i16) {
        let max_scroll = self.state.chat_max_scroll();
        let next_scroll = self
            .effective_chat_scroll()
            .saturating_add_signed(delta)
            .min(max_scroll);
        self.state.scroll = next_scroll;
        self.state.auto_scroll = delta > 0 && next_scroll == max_scroll;
    }

    pub(super) fn scroll_chat_to_top(&mut self) {
        self.state.auto_scroll = false;
        self.state.scroll = 0;
    }

    pub(super) fn scroll_chat_to_bottom(&mut self) {
        self.state.auto_scroll = true;
        self.state.scroll = self.state.chat_max_scroll();
    }

    pub(super) fn start_turn_timer(&mut self, now: Instant) {
        self.state.profile_lock_stale_after_terminal_event = false;
        self.state.turn_timer.start(now);
    }

    pub(super) fn start_or_resume_turn_timer(&mut self, now: Instant) {
        self.state.profile_lock_stale_after_terminal_event = false;
        if self.state.turn_timer.has_started() {
            self.state.turn_timer.resume(now);
        } else {
            self.state.turn_timer.start(now);
        }
    }

    pub(super) fn pause_turn_timer(&mut self, now: Instant) {
        self.state.turn_timer.pause(now);
    }

    pub(super) fn finish_turn_timer(&mut self, now: Instant) {
        self.state.turn_timer.finish(now);
    }

    pub(super) fn finish_terminal_turn_timer(&mut self, now: Instant) {
        if self.state.profile_locked {
            self.state.profile_lock_stale_after_terminal_event = true;
        }
        self.finish_turn_timer(now);
    }

    pub(super) fn clear_turn_timer(&mut self) {
        self.state.profile_lock_stale_after_terminal_event = false;
        self.state.turn_timer.clear();
    }

    pub(super) fn sync_turn_timer_with_activity(&mut self, now: Instant) {
        if self.state.runtime_is_active() {
            self.start_or_resume_turn_timer(now);
            return;
        }

        if !self.state.turn_timer.has_started() {
            return;
        }

        if self.state.profile_locked && self.state.tool_phase == ToolPhase::Deciding {
            self.state.turn_timer.pause(now);
        } else {
            self.state.turn_timer.finish(now);
        }
    }

    pub fn new(runtime: RuntimeHandle, startup_options: StartupOptions) -> Self {
        let event_rx = spawn_event_bridge(runtime.subscribe());
        let (runtime_tx, runtime_rx) = spawn_runtime_bridge(runtime);
        let state = AppState::from_startup_options(startup_options.clone());

        let mut app = Self {
            event_rx,
            runtime_tx,
            runtime_rx,
            clipboard: None,
            ci_output: Box::new(io::stdout()),
            ci_output_needs_newline: false,
            ci_turn_completion_pending: false,
            ci_metrics_history_pending: false,
            ci_metrics_context_pending: false,
            startup_options,
            startup_message_sent: false,
            ci_error: None,
            stream_event_content: HashMap::new(),
            state,
            last_stream_history_request: None,
            last_statusline_animation_tick: None,
            event_lag_session_resync_pending: false,
            event_lag_tools_resync_pending: false,
            runtime_bridge_connected: true,
            runtime_bridge_error: None,
        };

        app.request_sync();
        app
    }

    pub fn run_ci(&mut self) -> Result<()> {
        while !self.state.exit {
            crossbeam_channel::select! {
                recv(self.event_rx) -> event => match event {
                    Ok(message) => self.handle_runtime_event_bridge_message(message),
                    Err(_) => self.fail_ci("event channel closed"),
                },
                recv(self.runtime_rx) -> response => match response {
                    Ok(response) => self.handle_runtime_response(response),
                    Err(_) => self.fail_ci("runtime response channel closed"),
                },
                default(Duration::from_millis(100)) => {}
            }
        }

        if let Some(error) = self.ci_error.take() {
            Err(color_eyre::eyre::eyre!(error))
        } else {
            Ok(())
        }
    }

    pub fn run(&mut self, mut terminal: ratatui::DefaultTerminal) -> Result<()> {
        let mut needs_redraw = true;
        while !self.state.exit {
            needs_redraw |= self.process_events();
            let event_timeout = if needs_redraw {
                std::time::Duration::from_millis(0)
            } else if self.state.runtime_is_active() {
                STATUSLINE_ANIMATION_INTERVAL
            } else {
                std::time::Duration::from_millis(100)
            };
            needs_redraw |= self.handle_events(event_timeout)?;
            needs_redraw |= self.advance_statusline_animation(Instant::now());

            if !needs_redraw {
                continue;
            }

            if self.state.mode == UiMode::Chat && self.state.tool_phase == ToolPhase::Deciding {
                terminal.hide_cursor()?;
            } else {
                terminal.show_cursor()?;
            }

            terminal.draw(|frame| {
                let area = frame.area();
                if self.state.mode == UiMode::Chat {
                    let input_height = bottom_panel_height(&self.state, area);
                    let layout = Layout::vertical([
                        Constraint::Min(area.height.saturating_sub(input_height + 1)),
                        Constraint::Length(1),
                        Constraint::Length(input_height),
                    ])
                    .flex(Flex::End);
                    let [chat_area, _, _] = layout.areas(area);
                    self.state.refresh_chat_render_cache(chat_area.width);
                    self.update_chat_viewport(chat_area.height);
                }

                frame.render_widget(&self.state, area);

                if self.state.mode == UiMode::Chat {
                    let input_height = bottom_panel_height(&self.state, area);
                    let layout = Layout::vertical([
                        Constraint::Min(area.height.saturating_sub(input_height + 1)),
                        Constraint::Length(1),
                        Constraint::Length(input_height),
                    ])
                    .flex(Flex::End);
                    let [_chat_area, _status_area, input_area] = layout.areas(area);
                    self.state.input_width = input_area.width;

                    let (cursor_x, cursor_y) =
                        TextInput::new(&self.state.input, self.state.input_cursor)
                            .get_cursor_position(input_area);
                    frame.set_cursor_position((cursor_x, cursor_y));
                }
            })?;
            needs_redraw = false;
        }
        if let Some(error) = self.runtime_bridge_error.take() {
            Err(color_eyre::eyre::eyre!(error))
        } else {
            Ok(())
        }
    }

    pub fn exit_token_usage_summary(&self) -> Option<String> {
        if self.state.exit_usage_totals.usage_by_model.is_empty() {
            return None;
        }

        let mut lines = vec![String::from("Token usage since launch:")];
        let mut total = kraai_runtime::TokenUsage::default();

        for (model_key, usage) in &self.state.exit_usage_totals.usage_by_model {
            accumulate_token_usage(&mut total, usage);
            lines.push(format!(
                "  {}/{}: {}",
                model_key.provider_id,
                model_key.model_id,
                format_exit_usage_fields(usage)
            ));
        }

        lines.push(format!("  total: {}", format_exit_usage_fields(&total)));
        Some(lines.join("\n"))
    }

    pub fn evaluation_metrics(&self) -> serde_json::Value {
        let mut usage = kraai_runtime::TokenUsage::default();
        for model_usage in self.state.exit_usage_totals.usage_by_model.values() {
            accumulate_token_usage(&mut usage, model_usage);
        }
        let tool_calls = self
            .state
            .chat_history
            .values()
            .filter(|message| message.role == kraai_types::ChatRole::Tool)
            .count();
        let usage = (usage.total_tokens != 0
            || usage.input_tokens != 0
            || usage.output_tokens != 0
            || usage.reasoning_tokens != 0
            || usage.cache_read_tokens != 0)
            .then(|| {
                serde_json::json!({
                    "total_tokens": usage.total_tokens,
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "reasoning_tokens": usage.reasoning_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                })
            });
        serde_json::json!({
            "schema_version": 1,
            "turns": self.state.exit_usage_totals.completed_message_ids.len(),
            "tool_calls": tool_calls,
            "final_context_tokens": self.state.context_usage.as_ref().map(|context| context.used_context_tokens()),
            "usage": usage,
        })
    }

    pub(super) fn process_events(&mut self) -> bool {
        let mut changed = false;

        while let Ok(message) = self.event_rx.try_recv() {
            self.handle_runtime_event_bridge_message(message);
            changed = true;
        }

        loop {
            match self.runtime_rx.try_recv() {
                Ok(response) => {
                    self.handle_runtime_response(response);
                    changed = true;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.handle_runtime_bridge_disconnect();
                    changed = true;
                    break;
                }
            }
        }

        changed
    }

    pub(super) fn handle_runtime_event_bridge_message(
        &mut self,
        message: RuntimeEventBridgeMessage,
    ) {
        match message {
            RuntimeEventBridgeMessage::Event(event) => self.handle_runtime_event(event),
            RuntimeEventBridgeMessage::Lagged(skipped) => {
                self.event_lag_session_resync_pending = true;
                self.event_lag_tools_resync_pending = true;
                self.state.retry_waiting = false;
                self.last_stream_history_request = None;
                self.stream_event_content.clear();
                self.state.status =
                    format!("Missed {skipped} runtime event(s); resynchronizing session state");
                self.request_sync();
            }
        }
    }

    pub(super) fn advance_statusline_animation(&mut self, now: Instant) -> bool {
        if !self.state.runtime_is_active() {
            self.last_statusline_animation_tick = None;
            if self.state.statusline_animation_frame != 0 {
                self.state.statusline_animation_frame = 0;
                return true;
            }
            return false;
        }

        let Some(last_tick) = self.last_statusline_animation_tick else {
            self.last_statusline_animation_tick = Some(now);
            return false;
        };

        if now.duration_since(last_tick) < STATUSLINE_ANIMATION_INTERVAL {
            return false;
        }

        self.last_statusline_animation_tick = Some(now);
        self.state.statusline_animation_frame =
            (self.state.statusline_animation_frame + 1) % STATUSLINE_STREAMING_FRAMES.len();
        true
    }
}

fn accumulate_token_usage(
    total: &mut kraai_runtime::TokenUsage,
    usage: &kraai_runtime::TokenUsage,
) {
    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
    total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
    total.reasoning_tokens = total
        .reasoning_tokens
        .saturating_add(usage.reasoning_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(usage.cache_read_tokens);
}

fn format_exit_usage_fields(usage: &kraai_runtime::TokenUsage) -> String {
    format!(
        "total {}, input {}, output {}",
        format_cached_token_count(
            usage.total_tokens.saturating_sub(usage.cache_read_tokens),
            usage.cache_read_tokens
        ),
        format_cached_token_count(usage.input_tokens, usage.cache_read_tokens),
        format_reasoning_token_count(usage.output_tokens, usage.reasoning_tokens),
    )
}

fn format_cached_token_count(tokens: usize, cached_tokens: usize) -> String {
    if cached_tokens == 0 {
        return format_token_count(tokens);
    }

    format!(
        "{} (+{} cached)",
        format_token_count(tokens),
        format_token_count(cached_tokens)
    )
}

fn format_reasoning_token_count(output_tokens: usize, reasoning_tokens: usize) -> String {
    if reasoning_tokens == 0 {
        return format_token_count(output_tokens);
    }

    format!(
        "{} (+{} reasoning)",
        format_token_count(output_tokens),
        format_token_count(reasoning_tokens)
    )
}
