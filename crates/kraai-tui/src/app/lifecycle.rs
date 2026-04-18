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
        self.clear_chat_selection();
    }

    pub(super) fn scroll_chat_to_top(&mut self) {
        self.state.auto_scroll = false;
        self.state.scroll = 0;
        self.clear_chat_selection();
    }

    pub(super) fn scroll_chat_to_bottom(&mut self) {
        self.state.auto_scroll = true;
        self.state.scroll = self.state.chat_max_scroll();
        self.clear_chat_selection();
    }

    pub fn new(runtime: RuntimeHandle, startup_options: StartupOptions) -> Self {
        let event_rx = spawn_event_bridge(runtime.subscribe());
        let (runtime_tx, runtime_rx) = spawn_runtime_bridge(runtime);
        let state = AppState::from_startup_options(startup_options.clone());

        let app = Self {
            event_rx,
            runtime_tx,
            runtime_rx,
            clipboard: None,
            ci_output: Box::new(io::stdout()),
            ci_output_needs_newline: false,
            ci_turn_completion_pending: false,
            startup_options,
            startup_message_sent: false,
            ci_error: None,
            state,
            last_stream_refresh: None,
            last_statusline_animation_tick: None,
        };

        app.request_sync();
        app
    }

    pub fn run_ci(&mut self) -> Result<()> {
        while !self.state.exit {
            crossbeam_channel::select! {
                recv(self.event_rx) -> event => match event {
                    Ok(event) => self.handle_runtime_event(event),
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
                if self.state.mode == UiMode::Chat && self.state.tool_phase != ToolPhase::Deciding {
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
                    let view = {
                        let cache = self.state.chat_render_cache.borrow();
                        ChatHistory::visible_view_from_sections(
                            &cache.sections,
                            cache.total_lines,
                            chat_area,
                            self.state.scroll,
                            self.state.auto_scroll,
                        )
                    };
                    self.state.visible_chat_view = Some(view);
                } else {
                    self.state.visible_chat_view = None;
                }

                frame.render_widget(&self.state, area);

                if self.state.mode == UiMode::Chat && self.state.tool_phase != ToolPhase::Deciding {
                    let input_height = bottom_panel_height(&self.state, area);
                    let layout = Layout::vertical([
                        Constraint::Min(area.height.saturating_sub(input_height + 1)),
                        Constraint::Length(1),
                        Constraint::Length(input_height),
                    ])
                    .flex(Flex::End);
                    let [_chat_area, _status_area, input_area] = layout.areas(area);

                    let (cursor_x, cursor_y) =
                        TextInput::new(&self.state.input, self.state.input_cursor)
                            .get_cursor_position(input_area);
                    frame.set_cursor_position((cursor_x, cursor_y));
                }
            })?;
            needs_redraw = false;
        }
        Ok(())
    }

    pub(super) fn process_events(&mut self) -> bool {
        let mut changed = false;

        while let Ok(event) = self.event_rx.try_recv() {
            self.handle_runtime_event(event);
            changed = true;
        }

        while let Ok(response) = self.runtime_rx.try_recv() {
            self.handle_runtime_response(response);
            changed = true;
        }

        changed
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
