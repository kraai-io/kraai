use super::*;

const RUNTIME_EVENT_BATCH_BUDGET: Duration = Duration::from_millis(2);
const RUNTIME_EVENT_BATCH_ROUNDS: usize = 64;

impl App {
    pub(super) fn update_chat_viewport(&mut self, height: u16) {
        self.state.chat_viewport_height = height;
        self.clamp_chat_scroll();
    }

    pub(super) fn effective_chat_scroll(&self) -> usize {
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
            .saturating_add_signed(isize::from(delta))
            .min(max_scroll);
        self.state.scroll = next_scroll;
        self.state.auto_scroll = delta > 0 && next_scroll == max_scroll;
    }

    pub(super) fn scroll_chat_to_top(&mut self) {
        self.state.auto_scroll = false;
        self.state.scroll = 0;
    }

    pub(super) fn page_chat(&mut self, down: bool) {
        let lines = self.state.chat_viewport_height.saturating_sub(2).max(1);
        let lines = i16::try_from(lines).unwrap_or(i16::MAX);
        self.scroll_chat_by(if down { lines } else { -lines });
    }

    pub(super) fn scroll_chat_to_bottom(&mut self) {
        self.state.auto_scroll = true;
        self.state.scroll = self.state.chat_max_scroll();
    }

    pub fn new(runtime: RuntimeHandle, startup_options: StartupOptions) -> Self {
        let event_rx = spawn_event_bridge(runtime.clone());
        let (runtime_tx, runtime_rx) = spawn_runtime_bridge(runtime);
        let state = AppState::from_startup_options(&startup_options);

        Self {
            event_rx,
            runtime_tx,
            runtime_rx,
            input: input::TerminalInput::default(),
            hangup: crossbeam_channel::never(),
            clipboard: None,
            startup_options,
            persist_workspace_preferences: true,
            startup_message_sent: false,
            startup_sync: StartupSync::WaitingForRuntime,
            state,
            last_statusline_animation_tick: None,
            last_runtime_event_sequence: 0,
            last_session_event_sequences: HashMap::new(),
            session_snapshot_sequences: HashMap::new(),
            event_lag_session_resync_pending: false,
            event_lag_script_resync_pending: false,
            runtime_bridge_connected: true,
            runtime_bridge_error: None,
            runtime_event_batch_budget: RUNTIME_EVENT_BATCH_BUDGET,
        }
    }

    pub fn request_cursor_preferences(&mut self) {
        self.state.cursor_preferences.request();
    }

    pub fn run(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        cursor_override: &crate::terminal_cursor::CursorOverride,
    ) -> Result<()> {
        self.state.palette.request();
        let mut needs_redraw = true;
        let mut cursor = super::cursor::CursorBlink::new(Instant::now());
        let input = self.input.receiver().clone();
        self.hangup = crate::terminal_loss::watch()?;
        self.input.start()?;
        while !self.state.exit {
            needs_redraw |= self.process_events(|| Ok(!input.is_empty()))?;
            let now = Instant::now();
            let event_timeout = if needs_redraw {
                Some(Duration::ZERO)
            } else {
                let fading = self.state.palette.muted.is_some();
                let animation = self
                    .state
                    .runtime_is_active()
                    .then_some(STATUSLINE_ANIMATION_INTERVAL);
                let blink = (self.state.composer_cursor_enabled()
                    && self.state.cursor_preferences.custom_blink())
                .then(|| cursor.timeout(now));
                [self.state.feedback.timeout(now, fading), animation, blink]
                    .into_iter()
                    .flatten()
                    .min()
            };
            let prior_style = self.state.cursor_preferences.style;
            let prior_muted = self.state.palette.muted;
            let input_changed = self.handle_events(event_timeout)?;
            needs_redraw |= input_changed;
            needs_redraw |= prior_style != self.state.cursor_preferences.style;
            needs_redraw |= prior_muted != self.state.palette.muted;
            cursor_override.apply(self.state.cursor_preferences.style)?;
            if self.state.editor_requested {
                self.input.stop();
                let edited = self.open_composer_editor(terminal, cursor_override);
                self.input.start()?;
                if let Err(error) = edited {
                    if self.state.exit {
                        return Err(error);
                    }
                    self.set_error(format!("Editor error: {error}"));
                }
                cursor.update(Instant::now(), true);
                needs_redraw = true;
            }
            let blink_changed = cursor.update(Instant::now(), input_changed)
                && self.state.composer_cursor_enabled()
                && self.state.cursor_preferences.custom_blink();
            needs_redraw |= self.advance_statusline_animation(Instant::now());

            if !needs_redraw {
                if blink_changed {
                    cursor.show(terminal)?;
                }
                continue;
            }

            terminal.draw(|frame| {
                let area = frame.area();
                if matches!(self.state.mode, UiMode::Chat | UiMode::Executions) {
                    let [chat_area, _, _] = ui::chat_layout(&self.state, area);
                    self.state.refresh_chat_render_cache(chat_area.width);
                    self.update_chat_viewport(chat_area.height);
                }

                frame.render_widget(&self.state, area);

                cursor.position = None;
                if self.state.mode == UiMode::Chat {
                    let [_, _, input_area] = ui::chat_layout(&self.state, area);
                    self.state.input_width = input_area.width;

                    if self.state.composer_cursor_enabled()
                        && input_area.width > 0
                        && input_area.height > 0
                    {
                        cursor.position = Some(
                            TextInput::new(&self.state.input, self.state.input_cursor)
                                .get_cursor_position(input_area)
                                .into(),
                        );
                    }
                }
                if let Some(position) = cursor.position
                    && (!self.state.cursor_preferences.custom_blink() || cursor.visible)
                {
                    frame.set_cursor_position(position);
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
            let lines = self.exit_cost_summary();
            return (!lines.is_empty()).then(|| lines.join("\n"));
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
        lines.extend(self.exit_cost_summary());
        Some(lines.join("\n"))
    }

    pub(super) fn process_events(
        &mut self,
        mut input_pending: impl FnMut() -> std::io::Result<bool>,
    ) -> Result<bool> {
        let mut changed = false;
        let started = Instant::now();
        let budget = self.runtime_event_batch_budget;
        let budget_spent = |changed: bool| changed && started.elapsed() >= budget;
        for _ in 0..RUNTIME_EVENT_BATCH_ROUNDS {
            if budget_spent(changed) || input_pending()? {
                break;
            }
            let mut received = false;
            match self.event_rx.try_recv() {
                Ok(message) => {
                    self.handle_runtime_event_bridge_message(message);
                    changed = true;
                    received = true;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.event_rx = crossbeam_channel::never();
                }
            }
            if budget_spent(changed) || input_pending()? {
                break;
            }
            match self.runtime_rx.try_recv() {
                Ok(response) => {
                    self.handle_runtime_response(response);
                    changed = true;
                    received = true;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.runtime_rx = crossbeam_channel::never();
                    self.handle_runtime_bridge_disconnect();
                    changed = true;
                    break;
                }
            }
            if !received {
                break;
            }
        }

        Ok(changed)
    }

    pub(super) fn handle_runtime_event_bridge_message(
        &mut self,
        message: RuntimeEventBridgeMessage,
    ) {
        match message {
            RuntimeEventBridgeMessage::StartupComplete(result) => {
                if self.startup_sync != StartupSync::WaitingForRuntime {
                    return;
                }
                self.startup_sync = StartupSync::Synchronizing;
                let error = match result {
                    Ok(kraai_runtime::RuntimeStartupState::Failed(error)) => Some(error),
                    Err(error) => Some(error.to_string()),
                    Ok(_) => None,
                };
                if let Some(error) = error {
                    self.set_error(format!("Runtime error: {error}"));
                }
                self.request_sync();
                self.request(RuntimeRequest::FinishStartupSync);
            }
            RuntimeEventBridgeMessage::Event(event) => {
                if event.sequence <= self.last_runtime_event_sequence {
                    return;
                }
                self.last_runtime_event_sequence = event.sequence;
                let covered_by_snapshot = if let Some(session_id) = event.event.session_id() {
                    self.last_session_event_sequences
                        .insert(session_id.clone(), event.sequence);
                    self.session_snapshot_sequences
                        .get(session_id)
                        .is_some_and(|snapshot_sequence| event.sequence <= *snapshot_sequence)
                } else {
                    false
                };
                if covered_by_snapshot {
                    self.apply_runtime_event(event.event, false);
                } else {
                    self.handle_runtime_event(event.event);
                }
            }
            RuntimeEventBridgeMessage::Lagged(skipped) => {
                self.state.cost_recovery_list_pending = true;
                self.event_lag_session_resync_pending = true;
                self.event_lag_script_resync_pending = true;
                self.state.retry_waiting = false;
                self.state.status =
                    format!("Missed {skipped} runtime event(s); resynchronizing session state");
                if self.startup_sync != StartupSync::WaitingForRuntime {
                    self.request_sync();
                }
            }
        }
    }

    pub(super) fn advance_statusline_animation(&mut self, now: Instant) -> bool {
        let expired = self.state.feedback.expire(now);
        let expired =
            expired | (self.state.palette.muted.is_some() && self.state.feedback.advance_fade(now));
        if !self.state.runtime_is_active() {
            self.last_statusline_animation_tick = None;
            if self.state.statusline_animation_frame != 0 {
                self.state.statusline_animation_frame = 0;
                return true;
            }
            return expired;
        }

        let Some(last_tick) = self.last_statusline_animation_tick else {
            self.last_statusline_animation_tick = Some(now);
            return expired;
        };

        if now.duration_since(last_tick) < STATUSLINE_ANIMATION_INTERVAL {
            return expired;
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
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(usage.cache_write_tokens);
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
