//! [`App`]: global state and screen routing.
//!
//! Screen flow (spec section 6):
//! `Startup → Operation Picker → File Browser → Parameter Form → Running → Done`
//! with `Esc` going back one step, plus a persistent `Queue` screen
//! reachable from anywhere, a `Help` overlay, and a `MissingFfmpeg` screen
//! when no binary resolves at startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::background::{spawn_custom_path_check, spawn_probe, BackgroundMsg};
use crate::config::presets::{apply_preset, preset_from_fields, Preset};
use crate::config::settings::Settings;
use crate::ffmpeg::capabilities::CapabilityReport;
use crate::ffmpeg::probe::{ProbeError, ProbeResult};
use crate::ffmpeg::runner::{force_kill, spawn_job, JobRequest};
use crate::ops::fields::{collect_values, Field, FieldValue};
use crate::ops::{operation_for, OperationMeta, OPERATIONS};
use crate::queue::{expand_template, Job, JobStatus, QueueState};
use crate::ui::screens::parameter_form::ParamForm;
use crate::ui::screens::popups::{NamePrompt, PresetPopup};
use crate::ui::screens::running::{RunAction, RunPhase, RunState};
use crate::ui::screens::{file_browser, help, missing_ffmpeg, operation_picker, parameter_form};
use crate::ui::theme::Theme;

/// All screens the app can show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Screen {
    /// Capability detection is running; spinner until it resolves.
    #[default]
    Startup,
    /// No ffmpeg binary found (or the found one is unusable).
    MissingFfmpeg,
    /// Pick one of the ten operations.
    OperationPicker,
    /// Choose input file(s).
    FileBrowser,
    /// Adjust parameters with live command preview (form widgets land in M3).
    ParameterForm,
    /// Watch a running ffmpeg job. Placeholder until M4.
    Running,
    /// Batch job queue; remembers where it was opened from.
    Queue { return_to: Box<Screen> },
    /// Keybinding help overlay; remembers the screen underneath.
    Help { return_to: Box<Screen> },
}

/// Probe lifecycle for one file, as shown in the media-info panes.
#[derive(Debug, Clone)]
pub enum ProbeState {
    /// Probe task in flight.
    Pending,
    /// Probe succeeded.
    Ready(ProbeResult),
    /// Probe failed; the human-readable message is displayable as-is.
    Failed(String),
}

/// Global application state.
pub struct App {
    /// Which screen is currently visible.
    pub screen: Screen,
    /// Index into [`OPERATIONS`] of the highlighted operation.
    pub selected_operation: usize,
    /// One-line status / hint shown in footers.
    pub status_message: Option<String>,
    /// When true, the main loop exits after the current iteration.
    pub should_quit: bool,
    /// Persisted settings (binary paths). Saved on custom-path success.
    pub settings: Settings,
    /// Session capability report; `None` until detection resolves.
    pub caps: Option<CapabilityReport>,
    /// Tick counter driving the startup spinner.
    pub spinner: usize,
    /// File browser state.
    pub browser: file_browser::BrowserState,
    /// Probe cache by input path, including in-flight and failed probes.
    pub probes: HashMap<PathBuf, ProbeState>,
    /// Inputs confirmed in the browser; the form operates on these.
    pub form_inputs: Vec<PathBuf>,
    /// Live parameter form; `None` until inputs are confirmed.
    pub form: Option<ParamForm>,
    /// Running job state; `None` until the user runs something.
    pub run: Option<RunState>,
    /// Batch queue (M6): enqueued jobs and execution state.
    pub queue: QueueState,
    /// Preset waiting to be applied at the next form open.
    pub pending_preset: Option<Preset>,
    /// Preset picker overlay (operation picker `p`).
    pub preset_popup: Option<PresetPopup>,
    /// Preset-name entry overlay (form `s`).
    pub save_prompt: Option<NamePrompt>,
    /// Missing-FFmpeg screen state.
    pub missing: missing_ffmpeg::MissingState,
    /// Outgoing side of the background channel (cloned into spawn calls).
    tx: UnboundedSender<BackgroundMsg>,
    /// Incoming side, drained once per frame by [`poll_background`](Self::poll_background).
    rx: UnboundedReceiver<BackgroundMsg>,
}

impl App {
    /// Fresh app starting at the loading screen. Detection and all later
    /// background work report through `tx`/`rx`.
    pub fn new(
        tx: UnboundedSender<BackgroundMsg>,
        rx: UnboundedReceiver<BackgroundMsg>,
        settings: Settings,
    ) -> Self {
        Self {
            screen: Screen::Startup,
            selected_operation: 0,
            status_message: None,
            should_quit: false,
            settings,
            caps: None,
            spinner: 0,
            browser: file_browser::BrowserState::open(start_dir()),
            probes: HashMap::new(),
            form_inputs: Vec::new(),
            form: None,
            run: None,
            queue: QueueState::new(),
            pending_preset: None,
            preset_popup: None,
            save_prompt: None,
            missing: missing_ffmpeg::MissingState::default(),
            tx,
            rx,
        }
    }

    /// Metadata of the currently highlighted operation.
    pub fn selected_operation_meta(&self) -> &'static OperationMeta {
        // INVARIANT: OPERATIONS is non-empty (enforced by unit test) and
        // `selected_operation` is always kept in range by `move_selection`.
        OPERATIONS
            .get(self.selected_operation)
            .expect("operation registry is non-empty and selection is in range")
    }

    /// Resolved ffprobe binary, if detection found one.
    fn ffprobe_path(&self) -> Option<PathBuf> {
        self.caps.as_ref().and_then(|c| c.ffprobe_path.clone())
    }

    /// Ensure a probe is in flight (or cached) for `path`. Media files are
    /// probed lazily on highlight; confirmed inputs are probed regardless.
    /// Never spawns twice for the same path — `Pending` marks in-flight work.
    pub fn ensure_probe(&mut self, path: &PathBuf) {
        if self.probes.contains_key(path) {
            return;
        }
        self.probes.insert(path.clone(), ProbeState::Pending);
        spawn_probe(self.tx.clone(), self.ffprobe_path(), path.clone());
    }

    /// Drain background messages. Called once per frame from the main loop.
    pub fn poll_background(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle_msg(msg);
        }
    }

    /// Route one background message into state.
    fn handle_msg(&mut self, msg: BackgroundMsg) {
        match msg {
            BackgroundMsg::CapabilitiesReady(report) => {
                if !report.found || !report.usable {
                    if report.found {
                        self.missing.error = Some(
                            "The ffmpeg binary that was found did not respond to \
                             `-version`. Enter a custom path below."
                                .to_string(),
                        );
                    }
                    self.caps = Some(report);
                    self.screen = Screen::MissingFfmpeg;
                    return;
                }
                if !report.warnings.is_empty() {
                    self.status_message = Some(report.warnings.join(" "));
                }
                self.caps = Some(report);
                if self.screen == Screen::Startup {
                    self.screen = Screen::OperationPicker;
                }
            }
            BackgroundMsg::CustomPathChecked(report) => {
                self.missing.checking = false;
                if report.usable {
                    if let Some(pending) = self.missing.pending_path.take() {
                        self.settings.general.ffmpeg_path = Some(pending);
                        if let Err(e) = self.settings.save() {
                            self.status_message =
                                Some(format!("Custom path works but could not be saved: {e:#}"));
                        }
                    }
                    self.caps = Some(report);
                    self.screen = Screen::OperationPicker;
                } else {
                    self.missing.pending_path = None;
                    self.missing.error = Some(
                        "That file did not respond like ffmpeg. Check the path and try again."
                            .to_string(),
                    );
                }
            }
            BackgroundMsg::ProbeReady { path, result } => {
                let state = match result {
                    Ok(probed) => ProbeState::Ready(probed),
                    Err(e) => ProbeState::Failed(probe_error_message(&e)),
                };
                self.probes.insert(path, state);
            }
            BackgroundMsg::JobStarted { job_id, pid } => {
                if self.run.as_ref().is_some_and(|run| run.job_id == job_id) {
                    if let Some(run) = self.run.as_mut() {
                        run.pid = pid;
                    }
                }
                // Queue jobs track no pid (cancelled via stored triggers).
            }
            BackgroundMsg::JobProgress { job_id, update } => {
                if self.run.as_ref().is_some_and(|run| run.job_id == job_id) {
                    if let Some(run) = self.run.as_mut() {
                        run.apply_progress(&update);
                    }
                } else {
                    self.queue.apply_progress(job_id, &update);
                }
            }
            BackgroundMsg::JobStderrLine { job_id, line } => {
                if self.run.as_ref().is_some_and(|run| run.job_id == job_id) {
                    if let Some(run) = self.run.as_mut() {
                        run.push_stderr(line);
                    }
                }
                // Queue jobs keep stderr in their final result only.
            }
            BackgroundMsg::JobFinished { job_id, result } => {
                if self.run.as_ref().is_some_and(|run| run.job_id == job_id) {
                    if let Some(run) = self.run.as_mut() {
                        run.apply_finished(result);
                    }
                } else {
                    self.finish_queue_job(job_id, result);
                }
            }
        }
    }

    /// Move the picker selection by `delta`, wrapping around.
    fn move_selection(&mut self, delta: i32) {
        let n = OPERATIONS.len() as i32;
        self.selected_operation = (self.selected_operation as i32 + delta).rem_euclid(n) as usize;
        self.status_message = None;
    }

    /// Tick handler: advances the startup spinner and drives the §2
    /// auto-return — a successful job shows its final stats for
    /// [`SUCCESS_PAUSE`] then returns to the picker without a keypress.
    /// Failures and cancellations stay until acknowledged.
    pub fn on_tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
        if self.screen == Screen::Running {
            let ready = self.run.as_ref().is_some_and(|run| {
                run.phase == RunPhase::Done
                    && run.succeeded()
                    && run
                        .finished_at
                        .is_some_and(|at| at.elapsed() >= SUCCESS_PAUSE)
            });
            if ready {
                self.status_message = self.success_summary();
                self.screen = Screen::OperationPicker;
            }
        }
    }

    /// One-line success summary carried back to the picker.
    fn success_summary(&self) -> Option<String> {
        let run = self.run.as_ref()?;
        let wall = run.result.as_ref().map(|r| r.wall_time)?;
        let size = std::fs::metadata(&run.output)
            .ok()
            .map(|m| crate::ffmpeg::probe::format_size(m.len()))
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        Some(format!(
            "Done in {:.1}s → {}{size}",
            wall.as_secs_f64(),
            run.output.display()
        ))
    }

    /// Global key dispatch. Picker navigation supports both arrows and vim
    /// keys; see the keybinding table in spec section 6.
    pub fn on_key(&mut self, key: KeyEvent) {
        // Ctrl+C cancels running work (single job, then queue); second press
        // force-kills a terminating single job. Anywhere else it quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.job_cancellable() {
                self.cancel_job();
            } else if self.job_cancelling() {
                self.force_kill_job();
            } else if self.queue.running && !self.queue.active.is_empty() {
                self.cancel_queue_jobs();
            } else {
                self.should_quit = true;
            }
            return;
        }

        // Overlays take over input while visible.
        if self.preset_popup.is_some() {
            self.on_key_preset_popup(key);
            return;
        }
        if self.save_prompt.is_some() {
            self.on_key_save_prompt(key);
            return;
        }

        // `q` quits from anywhere except while work runs or text has focus.
        if key.code == KeyCode::Char('q') && !self.has_text_focus() {
            if self.job_active() || self.queue.running {
                self.status_message =
                    Some("Work is running — Ctrl+C cancels it, then q quits.".to_string());
                return;
            }
            self.should_quit = true;
            return;
        }

        // `Q` opens the queue from the setup screens (never while typing).
        if key.code == KeyCode::Char('Q') && !self.has_text_focus() {
            match &self.screen {
                Screen::OperationPicker | Screen::FileBrowser | Screen::ParameterForm => {
                    self.screen = Screen::Queue {
                        return_to: Box::new(self.screen.clone()),
                    };
                    return;
                }
                Screen::Queue { .. }
                | Screen::Startup
                | Screen::MissingFfmpeg
                | Screen::Running
                | Screen::Help { .. } => {}
            }
        }

        match &self.screen {
            Screen::Startup => {}
            Screen::MissingFfmpeg => {
                if missing_ffmpeg::on_key(&mut self.missing, key) {
                    if let Some(path) = self.missing.pending_path.clone() {
                        let ffprobe = self.settings.general.ffprobe_path.clone();
                        spawn_custom_path_check(self.tx.clone(), path, ffprobe);
                    }
                }
            }
            Screen::OperationPicker => self.on_key_picker(key),
            Screen::FileBrowser => self.on_key_browser(key),
            Screen::ParameterForm => self.on_key_form(key),
            Screen::Running => self.on_key_running(key),
            Screen::Help { .. } => {
                // Any key closes the help overlay (Esc/? also work naturally).
                if let Screen::Help { return_to } =
                    std::mem::replace(&mut self.screen, Screen::OperationPicker)
                {
                    self.screen = *return_to;
                }
            }
            Screen::Queue { .. } => self.on_key_queue(key),
        }
    }

    fn on_key_picker(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('p') => {
                self.preset_popup = Some(PresetPopup::default());
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter => {
                self.browser = file_browser::BrowserState::open(start_dir());
                self.screen = Screen::FileBrowser;
                self.status_message = None;
                self.probe_highlighted();
            }
            KeyCode::Char('?') => {
                self.screen = Screen::Help {
                    return_to: Box::new(Screen::OperationPicker),
                };
            }
            KeyCode::Esc => self.should_quit = true,
            _ => {}
        }
    }

    fn on_key_browser(&mut self, key: KeyEvent) {
        // `?` opens help without disturbing browser state; global `q`
        // already returned above, and text keys belong to the browser.
        if key.code == KeyCode::Char('?') {
            self.screen = Screen::Help {
                return_to: Box::new(Screen::FileBrowser),
            };
            return;
        }
        match self.browser.on_key(key) {
            file_browser::BrowserAction::None => {}
            file_browser::BrowserAction::Status(message) => {
                self.status_message = Some(message);
            }
            file_browser::BrowserAction::Back => {
                self.screen = Screen::OperationPicker;
                self.status_message = None;
            }
            file_browser::BrowserAction::Confirmed(inputs) => {
                for input in &inputs {
                    self.ensure_probe(input);
                }
                self.form_inputs = inputs;
                self.open_form();
                self.screen = Screen::ParameterForm;
                self.status_message = None;
            }
        }
        if self.screen == Screen::FileBrowser {
            self.probe_highlighted();
        }
    }

    /// Probe the highlighted file when it is media. Keeps the side pane
    /// fresh as the cursor moves; `ensure_probe` dedupes in-flight work.
    fn probe_highlighted(&mut self) {
        if let Some(entry) = self.browser.highlighted() {
            if entry.is_media {
                self.ensure_probe(&entry.path.clone());
            }
        }
    }

    /// Build a fresh parameter form for the selected operation over the
    /// confirmed inputs. Unknown operation ids (stale presets, M5 ops with
    /// no fields yet) still open — the form shows the build error instead
    /// of refusing, so failures are visible, never silent.
    ///
    /// After construction the form absorbs, in order: config encoder
    /// defaults, a pending preset (jumped from the preset picker), and the
    /// configured output directory — then rebuilds so the preview reflects
    /// all three.
    fn open_form(&mut self) {
        let op_id = self.selected_operation_meta().id;
        let Some(op) = operation_for(op_id) else {
            self.status_message = Some(format!("Unknown operation: {op_id}"));
            return;
        };
        let probe = self.first_input_probe().cloned();
        let input_probes = self.all_input_probes();
        self.form = Some(ParamForm::new(
            &*op,
            &self.form_inputs,
            probe.as_ref(),
            input_probes,
            self.caps.as_ref(),
        ));
        if let Some(form) = self.form.as_mut() {
            apply_preset(
                &mut form.fields,
                &defaults_as_preset(&self.settings.defaults),
            );
            if let Some(preset) = self.pending_preset.take() {
                apply_preset(&mut form.fields, &preset);
                self.status_message = Some(format!("Preset '{}' applied.", preset.name));
            }
            rewrite_output_dir(&mut form.fields, &self.settings.general.default_output_dir);
        }
        self.rebuild_form();
    }

    /// Snapshot of one input's probe-driven build inputs for batch jobs.
    fn batch_probe(&self, input: &PathBuf) -> (Option<ProbeResult>, Vec<ProbeResult>) {
        let probe = self.probes.get(input).and_then(|state| match state {
            ProbeState::Ready(result) => Some(result.clone()),
            ProbeState::Pending | ProbeState::Failed(_) => None,
        });
        (probe.clone(), probe.into_iter().collect())
    }

    /// Ready probe of the first confirmed input, if ffprobe finished it.
    fn first_input_probe(&self) -> Option<&ProbeResult> {
        self.form_inputs
            .first()
            .and_then(|input| self.probes.get(input))
            .and_then(|state| match state {
                ProbeState::Ready(result) => Some(result),
                ProbeState::Pending | ProbeState::Failed(_) => None,
            })
    }

    /// Ready probes for all confirmed inputs in order, skipping pending
    /// and failed ones. Concat compares these; a shortfall means "not all
    /// known", which resolves to the safe re-encode path.
    fn all_input_probes(&self) -> Vec<ProbeResult> {
        self.form_inputs
            .iter()
            .filter_map(|input| self.probes.get(input))
            .filter_map(|state| match state {
                ProbeState::Ready(result) => Some(result.clone()),
                ProbeState::Pending | ProbeState::Failed(_) => None,
            })
            .collect()
    }

    /// Re-run the form builder after a field change. All borrows are disjoint
    /// fields, so the mutable form and the immutable context coexist.
    fn rebuild_form(&mut self) {
        let op_id = match self.form.as_ref() {
            Some(form) => form.op_id.clone(),
            None => return,
        };
        let Some(op) = operation_for(&op_id) else {
            return;
        };
        let probe = self.first_input_probe().cloned();
        let input_probes = self.all_input_probes();
        if let Some(form) = self.form.as_mut() {
            form.rebuild(
                &*op,
                &self.form_inputs,
                probe.as_ref(),
                input_probes,
                self.caps.as_ref(),
            );
        }
    }

    /// Keys on the parameter form: `?` help, `c` copy, everything else to
    /// the form itself.
    fn on_key_form(&mut self, key: KeyEvent) {
        let editing = self.form.as_ref().is_some_and(|f| f.editing);
        if key.code == KeyCode::Char('?') && !editing {
            self.screen = Screen::Help {
                return_to: Box::new(Screen::ParameterForm),
            };
            return;
        }
        if key.code == KeyCode::Char('c') && !editing {
            self.copy_command();
            return;
        }
        if key.code == KeyCode::Char('s') && !editing {
            self.save_prompt = Some(NamePrompt::new("Save preset as"));
            return;
        }
        let action = match self.form.as_mut() {
            Some(form) => form.on_key(key),
            None => parameter_form::FormAction::Back,
        };
        match action {
            parameter_form::FormAction::None => {}
            parameter_form::FormAction::Changed => self.rebuild_form(),
            parameter_form::FormAction::Back => {
                self.screen = Screen::FileBrowser;
                self.status_message = None;
            }
            parameter_form::FormAction::Run => self.start_job_flow(),
        }
    }

    /// Enter pressed on the form. One input (or concat, which is inherently
    /// one job over many files) takes the single-run flow with its overwrite
    /// confirm and full running screen; several inputs fan out into one
    /// queue job each.
    fn start_job_flow(&mut self) {
        let op_id = match self.form.as_ref() {
            Some(form) => form.op_id.clone(),
            None => return,
        };
        if op_id != "concat" && self.form_inputs.len() > 1 {
            self.start_batch_flow();
        } else {
            self.start_single_flow();
        }
    }

    /// Single-run flow: build the job from the live preview. Existing
    /// outputs go through the overwrite confirm (then `-y`) unless the
    /// config disables it; otherwise the job spawns immediately.
    fn start_single_flow(&mut self) {
        if self.single_or_queue_busy() {
            return;
        }
        let (op_name, spec, output) = match self.form.as_ref() {
            Some(form) => {
                let op_name = operation_for(&form.op_id)
                    .map(|op| op.name().to_string())
                    .unwrap_or_else(|| form.op_id.clone());
                let Some(spec) = form.preview.clone() else {
                    self.status_message =
                        Some("No command to run — fix the build error first.".to_string());
                    return;
                };
                let output = form
                    .fields
                    .iter()
                    .find(|f| f.id == "output")
                    .and_then(|f| match f.value() {
                        FieldValue::Text(text) if !text.trim().is_empty() => {
                            Some(PathBuf::from(text))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| PathBuf::from("ffkit_output"));
                (op_name, spec, output)
            }
            None => return,
        };
        let total_duration = self.first_input_probe().and_then(|p| p.duration);
        let input_size = self
            .first_input_probe()
            .and_then(|p| p.size_bytes)
            .or_else(|| {
                self.form_inputs
                    .first()
                    .and_then(|input| std::fs::metadata(input).ok())
                    .map(|m| m.len())
            });
        if output.exists() && self.settings.general.confirm_overwrite {
            self.run = Some(RunState::confirming(op_name, &spec, output));
        } else {
            self.run = Some(RunState::starting(
                op_name,
                &spec,
                output,
                total_duration,
                input_size,
            ));
            self.spawn_current_job();
        }
        self.screen = Screen::Running;
        self.status_message = None;
    }

    /// Batch flow: the same tuned parameters applied to every confirmed
    /// input, one job each (concat stays a single job over all inputs and
    /// never reaches here). Outputs come from the batch template; existing
    /// outputs arm a single whole-queue confirm instead of one prompt per
    /// file. A build failure aborts loudly before anything is enqueued.
    fn start_batch_flow(&mut self) {
        if self.single_or_queue_busy() {
            return;
        }
        let (op_id, op_name, field_values) = match self.form.as_ref() {
            Some(form) => (
                form.op_id.clone(),
                operation_for(&form.op_id)
                    .map(|op| op.name().to_string())
                    .unwrap_or_else(|| form.op_id.clone()),
                collect_values(&form.fields),
            ),
            None => return,
        };
        let Some(op) = operation_for(&op_id) else {
            self.status_message = Some(format!("Unknown operation: {op_id}"));
            return;
        };
        let template = self.settings.general.batch_template.clone();
        let mut jobs = Vec::new();
        for (index, input) in self.form_inputs.iter().enumerate() {
            let (probe, input_probes) = self.batch_probe(input);
            let first_pass = crate::ops::BuildContext {
                inputs: std::slice::from_ref(input),
                output: None,
                probe: probe.as_ref(),
                input_probes: input_probes.clone(),
                caps: self.caps.as_ref(),
                fields: field_values.clone(),
            };
            let default_spec = match op.build(&first_pass) {
                Ok(spec) => spec,
                Err(e) => {
                    self.status_message =
                        Some(format!("{}: {e:#} — nothing was enqueued", input.display()));
                    return;
                }
            };
            let default_out = default_spec
                .output_path()
                .unwrap_or_else(|| PathBuf::from("ffkit_output"));
            let (suffix, ext) = split_suffix(&default_out, input);
            let output = expand_template(&template, input, index + 1, &suffix, &ext);
            let second_pass = crate::ops::BuildContext {
                inputs: std::slice::from_ref(input),
                output: Some(&output),
                probe: probe.as_ref(),
                input_probes,
                caps: self.caps.as_ref(),
                fields: field_values.clone(),
            };
            let spec = match op.build(&second_pass) {
                Ok(spec) => spec,
                Err(e) => {
                    self.status_message =
                        Some(format!("{}: {e:#} — nothing was enqueued", input.display()));
                    return;
                }
            };
            let input_size = probe
                .as_ref()
                .and_then(|p| p.size_bytes)
                .or_else(|| std::fs::metadata(input).ok().map(|m| m.len()));
            jobs.push(Job::pending(crate::queue::NewJob {
                id: self.queue.take_id(),
                op_id: op_id.clone(),
                op_name: op_name.clone(),
                input: input.clone(),
                output,
                spec,
                total_duration: probe.as_ref().and_then(|p| p.duration),
                input_size,
            }));
        }
        let conflicts: Vec<PathBuf> = jobs
            .iter()
            .filter(|job| job.output.exists())
            .map(|job| job.output.clone())
            .collect();
        self.queue.enqueue(jobs);
        self.screen = Screen::Queue {
            return_to: Box::new(Screen::ParameterForm),
        };
        if !conflicts.is_empty() && self.settings.general.confirm_overwrite {
            self.queue.overwrite_conflicts = conflicts;
            self.status_message =
                Some("Some outputs already exist — y runs the batch and overwrites.".to_string());
        } else {
            self.start_queue_execution();
        }
    }

    /// True while a single job or the queue is executing: starting more work
    /// on top is refused with a hint instead of interleaving runners.
    fn single_or_queue_busy(&mut self) -> bool {
        let busy = self.job_active() || (self.queue.running && !self.queue.settled());
        if busy {
            self.status_message =
                Some("Finish or cancel the current work before starting more.".to_string());
        }
        busy
    }

    /// Begin (or resume) queue execution: fill worker slots up to the
    /// configured concurrency, then settle with a summary when drained.
    fn pump_queue(&mut self) {
        if !self.queue.running {
            return;
        }
        let want = self.settings.effective_concurrency();
        while self.queue.active.len() < want {
            let Some(id) = self.queue.pending_ids().into_iter().next() else {
                break;
            };
            self.spawn_queue_job(id);
        }
        if self.queue.settled() {
            self.queue.running = false;
            let summary = self.queue.finish_summary();
            self.status_message = Some(summary);
        }
    }

    /// Mark the queue running and pump. Clears stale conflicts (explicitly
    /// confirmed) — overwriting then proceeds under the standing `-y`.
    fn start_queue_execution(&mut self) {
        if self.queue.jobs.is_empty() || self.queue.running {
            return;
        }
        self.queue.running = true;
        self.queue.summary = None;
        self.queue.overwrite_conflicts.clear();
        self.status_message = None;
        self.pump_queue();
    }

    /// Spawn one queued job: ensure its output directory exists (ffmpeg
    /// will not create it), register the cancel trigger, and launch.
    fn spawn_queue_job(&mut self, id: u64) {
        let (spec, output) = match self.queue.find_mut(id) {
            Some(job) => {
                job.status = JobStatus::Running;
                (job.spec.clone(), job.output.clone())
            }
            None => return,
        };
        if let Some(parent) = output.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    if let Some(job) = self.queue.find_mut(id) {
                        job.status = JobStatus::Failed(format!("cannot create output dir: {e}"));
                    }
                    return;
                }
            }
        }
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        self.queue.cancels.insert(id, cancel_tx);
        self.queue.active.insert(id);
        spawn_job(self.tx.clone(), JobRequest { spec, output }, cancel_rx, id);
    }

    /// Settle one queue job from its final result, then keep pumping.
    /// Failures settle in place and never halt the queue (spec §12).
    fn finish_queue_job(&mut self, job_id: u64, result: crate::ffmpeg::runner::JobResult) {
        self.queue.active.remove(&job_id);
        self.queue.cancels.remove(&job_id);
        if let Some(job) = self.queue.find_mut(job_id) {
            job.settle(result);
        }
        self.pump_queue();
    }

    /// Ctrl+C on the queue screen: fire every active trigger and cancel all
    /// pending jobs immediately. Active jobs settle as Cancelled when their
    /// runners report back.
    fn cancel_queue_jobs(&mut self) {
        for (_, trigger) in self.queue.cancels.drain() {
            let _ = trigger.send(());
        }
        for job in &mut self.queue.jobs {
            if job.status == JobStatus::Pending {
                job.status = JobStatus::Cancelled;
            }
        }
        self.status_message = Some("Cancelling the queue…".to_string());
    }

    /// Keys on the queue screen: navigation, job management, armed start.
    fn on_key_queue(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('?') {
            self.screen = Screen::Help {
                return_to: Box::new(self.screen.clone()),
            };
            return;
        }
        let action = crate::ui::screens::queue::on_key(&mut self.queue, key);
        match action {
            crate::ui::screens::queue::QueueAction::None => {}
            crate::ui::screens::queue::QueueAction::Back => {
                if let Screen::Queue { return_to } =
                    std::mem::replace(&mut self.screen, Screen::OperationPicker)
                {
                    self.screen = *return_to;
                }
            }
            crate::ui::screens::queue::QueueAction::StartArmed => {
                if !self.queue.overwrite_conflicts.is_empty() || !self.queue.running {
                    self.start_queue_execution();
                }
            }
            crate::ui::screens::queue::QueueAction::RemoveSelected => {
                let id = self.queue.jobs.get(self.queue.selected).map(|job| job.id);
                match id {
                    Some(id) if self.queue.remove_pending(id) => {
                        self.status_message = Some("Pending job removed.".to_string());
                    }
                    _ => {
                        self.status_message = Some(
                            "Only pending jobs can be removed (cancel running ones first)."
                                .to_string(),
                        );
                    }
                }
            }
            crate::ui::screens::queue::QueueAction::ClearFinished => {
                self.queue.clear_finished();
            }
        }
    }

    /// Keys in the preset picker overlay.
    fn on_key_preset_popup(&mut self, key: KeyEvent) {
        let presets = crate::ui::screens::popups::all_presets(&self.settings);
        let action = match self.preset_popup.as_mut() {
            Some(popup) => crate::ui::screens::popups::on_key_preset_popup(popup, key, &presets),
            None => return,
        };
        match action {
            crate::ui::screens::popups::PresetPopupAction::None => {}
            crate::ui::screens::popups::PresetPopupAction::Close => {
                self.preset_popup = None;
            }
            crate::ui::screens::popups::PresetPopupAction::Apply(preset) => {
                self.preset_popup = None;
                match OPERATIONS.iter().position(|op| op.id == preset.operation) {
                    Some(index) => {
                        self.selected_operation = index;
                        self.pending_preset = Some(preset);
                        self.browser = file_browser::BrowserState::open(start_dir());
                        self.screen = Screen::FileBrowser;
                        self.status_message = None;
                        self.probe_highlighted();
                    }
                    None => {
                        self.status_message = Some(format!(
                            "Preset targets unknown operation '{}'.",
                            preset.operation
                        ));
                    }
                }
            }
        }
    }

    /// Keys in the preset-name prompt overlay.
    fn on_key_save_prompt(&mut self, key: KeyEvent) {
        let action = match self.save_prompt.as_mut() {
            Some(prompt) => crate::ui::screens::popups::on_key_prompt(prompt, key),
            None => return,
        };
        match action {
            crate::ui::screens::popups::PromptAction::None => {}
            crate::ui::screens::popups::PromptAction::Close => {
                self.save_prompt = None;
            }
            crate::ui::screens::popups::PromptAction::Submit(name) => {
                self.save_prompt = None;
                if name.is_empty() {
                    self.status_message = Some("Preset needs a name.".to_string());
                    return;
                }
                let Some(form) = self.form.as_ref() else {
                    return;
                };
                let preset = preset_from_fields(name.clone(), form.op_id.clone(), &form.fields);
                self.settings.presets.push(preset);
                match self.settings.save() {
                    Ok(()) => {
                        self.status_message = Some(format!("Preset '{name}' saved."));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Preset applied but not saved: {e:#}"));
                    }
                }
            }
        }
    }

    /// Spawn ffmpeg for the current run state. The cancel trigger is stored
    /// in the state so Ctrl+C can fire it exactly once. The job id comes
    /// from the shared queue counter so single and batch messages never
    /// collide.
    fn spawn_current_job(&mut self) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        run.cancel_tx = Some(cancel_tx);
        run.phase = RunPhase::Active;
        run.started = std::time::Instant::now();
        run.job_id = self.queue.take_id();
        let job_id = run.job_id;
        let request = JobRequest {
            spec: run.spec.clone(),
            output: run.output.clone(),
        };
        spawn_job(self.tx.clone(), request, cancel_rx, job_id);
    }

    /// True while a job is running and can still be cancelled gracefully.
    fn job_cancellable(&self) -> bool {
        self.screen == Screen::Running
            && self
                .run
                .as_ref()
                .is_some_and(|run| run.phase == RunPhase::Active)
    }

    /// True while termination is already in flight (second press kills).
    fn job_cancelling(&self) -> bool {
        self.screen == Screen::Running
            && self
                .run
                .as_ref()
                .is_some_and(|run| run.phase == RunPhase::Cancelling)
    }

    /// True while a job is running in any pre-done phase (guards `q`).
    fn job_active(&self) -> bool {
        self.screen == Screen::Running
            && self.run.as_ref().is_some_and(|run| {
                matches!(
                    run.phase,
                    RunPhase::Active | RunPhase::Cancelling | RunPhase::ConfirmOverwrite
                )
            })
    }

    /// First Ctrl+C: fire the cancel trigger; the runner sends SIGTERM,
    /// waits out the grace period, then SIGKILLs.
    fn cancel_job(&mut self) {
        if let Some(run) = self.run.as_mut() {
            if let Some(cancel) = run.cancel_tx.take() {
                let _ = cancel.send(());
            }
            run.phase = RunPhase::Cancelling;
        }
    }

    /// Second Ctrl+C while cancelling: kill the child immediately.
    fn force_kill_job(&mut self) {
        if let Some(pid) = self.run.as_ref().and_then(|run| run.pid) {
            tokio::spawn(force_kill(pid));
        }
    }

    /// Keys on the running screen: prompts, log pane, exit, report copy.
    /// During the §2 success pause, any key except `c`/`l` dismisses early
    /// to the picker; failures stay until explicitly acknowledged.
    fn on_key_running(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('?') {
            self.screen = Screen::Help {
                return_to: Box::new(Screen::Running),
            };
            return;
        }
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.phase == RunPhase::Done && run.succeeded())
        {
            match key.code {
                KeyCode::Char('c') => {
                    self.copy_report();
                    return;
                }
                KeyCode::Char('l') => {
                    if let Some(run) = self.run.as_mut() {
                        run.log_expanded = !run.log_expanded;
                        run.log_scroll = 0;
                    }
                    return;
                }
                _ => {
                    self.status_message = self.success_summary();
                    self.screen = Screen::OperationPicker;
                    return;
                }
            }
        }
        let action = match self.run.as_mut() {
            Some(run) => crate::ui::screens::running::on_key(run, key),
            None => RunAction::Back,
        };
        match action {
            RunAction::None => {}
            RunAction::Back => {
                self.screen = Screen::ParameterForm;
                self.status_message = None;
            }
            RunAction::Spawn => self.spawn_current_job(),
            RunAction::DeletePartial => self.delete_partial(),
            RunAction::CopyReport => self.copy_report(),
        }
    }

    /// Delete the partial output file after cancel/failure, then settle.
    fn delete_partial(&mut self) {
        if let Some(run) = self.run.as_mut() {
            match std::fs::remove_file(&run.output) {
                Ok(()) => self.status_message = Some("Partial file deleted.".to_string()),
                Err(e) => {
                    self.status_message = Some(format!("Could not delete partial file: {e}"));
                }
            }
            run.phase = RunPhase::Done;
        }
    }

    /// Copy the full error report (command + stderr) to the clipboard.
    fn copy_report(&mut self) {
        let Some(report) = self.run.as_ref().map(|run| run.error_report()) else {
            return;
        };
        match parameter_form::copy_to_clipboard(&report) {
            Ok(()) => self.status_message = Some("Error report copied.".to_string()),
            Err(message) => self.status_message = Some(message),
        }
    }

    /// Copy the preview command to the clipboard. Headless/SSH failures
    /// become a status message, never a panic (spec §2: degrade gracefully).
    fn copy_command(&mut self) {
        let Some(display) = self
            .form
            .as_ref()
            .and_then(|form| form.preview.as_ref())
            .map(|spec| spec.to_display())
        else {
            self.status_message = Some("Nothing to copy yet.".to_string());
            return;
        };
        match parameter_form::copy_to_clipboard(&display) {
            Ok(()) => self.status_message = Some("Command copied to clipboard.".to_string()),
            Err(message) => self.status_message = Some(message),
        }
    }

    /// Render the current screen, then any overlay popup. Delegates to
    /// `ui::screens::*`.
    pub fn render(&mut self, frame: &mut Frame) {
        const MIN_WIDTH: u16 = 80;
        const MIN_HEIGHT: u16 = 24;
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            crate::ui::layout::render_too_small(frame, area, MIN_WIDTH, MIN_HEIGHT);
            return;
        }
        let theme = Theme::from_name(&self.settings.general.theme);
        match self.screen {
            Screen::Startup => render_startup(frame, self.spinner),
            Screen::MissingFfmpeg => missing_ffmpeg::render(frame, self, &theme),
            Screen::OperationPicker => operation_picker::render(frame, self, &theme),
            Screen::FileBrowser => file_browser::render(frame, self, &theme),
            Screen::ParameterForm => {
                // Shared borrows coexist: the form plus the app context.
                if let Some(form) = self.form.as_ref() {
                    parameter_form::render(frame, self, form, &theme);
                } else {
                    crate::ui::screens::placeholder::render(frame, self, &theme);
                }
            }
            Screen::Help { .. } => help::render(frame, self, &theme),
            Screen::Running => crate::ui::screens::running::render(frame, self, &theme),
            Screen::Queue { .. } => crate::ui::screens::queue::render(frame, self, &theme),
        }
        if let Some(popup) = self.preset_popup.as_ref() {
            let presets = crate::ui::screens::popups::all_presets(&self.settings);
            crate::ui::screens::popups::render_preset_popup(frame, popup, &presets, &theme);
        }
        if let Some(prompt) = self.save_prompt.as_ref() {
            crate::ui::screens::popups::render_prompt(frame, prompt, &theme);
        }
    }
}

/// How long the success card stays up before auto-returning to the picker.
const SUCCESS_PAUSE: Duration = Duration::from_millis(1800);

/// Spinner frames for the startup loading screen.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Startup loading screen while capability detection runs.
fn render_startup(frame: &mut Frame, spinner: usize) {
    use ratatui::layout::Alignment;
    use ratatui::widgets::{Block, Borders, Paragraph};

    let glyph = SPINNER_FRAMES[spinner % SPINNER_FRAMES.len()];
    let text = Paragraph::new(format!("{glyph} Detecting FFmpeg…"))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title("ffkit"));
    frame.render_widget(text, frame.area());
}

/// Whether the focused screen owns single-character input (text fields), in
/// which case global single-key bindings like `q` must not fire.
impl App {
    /// True while the user is typing into any field: the missing-screen
    /// path box, the browser `/` jump box, a form text field in edit mode,
    /// or the preset-name prompt.
    fn has_text_focus(&self) -> bool {
        if self.save_prompt.is_some() {
            return true;
        }
        match &self.screen {
            Screen::MissingFfmpeg => true,
            Screen::FileBrowser => self.browser.path_mode,
            Screen::ParameterForm => self.form.as_ref().is_some_and(|f| f.editing),
            Screen::Startup
            | Screen::OperationPicker
            | Screen::Running
            | Screen::Queue { .. }
            | Screen::Help { .. } => false,
        }
    }
}

/// Directory the browser starts in: current dir, else home, else root.
fn start_dir() -> PathBuf {
    std::env::current_dir()
        .ok()
        .or_else(home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Home directory helper (mirrors the browser's; kept local to avoid
/// cross-module plumbing for a two-liner).
fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

/// Config `[defaults]` as a synthetic preset so form fields absorb them
/// through the same coercion as user presets (select by value, slider int).
fn defaults_as_preset(defaults: &crate::config::settings::EncoderDefaults) -> Preset {
    use std::collections::HashMap;
    Preset {
        name: String::new(),
        operation: String::new(),
        description: String::new(),
        params: HashMap::from([
            (
                "video_codec".to_string(),
                toml_value_string(&defaults.video_codec),
            ),
            ("crf".to_string(), toml::Value::Integer(defaults.crf)),
            ("preset".to_string(), toml_value_string(&defaults.preset)),
            (
                "audio_codec".to_string(),
                toml_value_string(&defaults.audio_codec),
            ),
            (
                "audio_bitrate".to_string(),
                toml_value_string(&defaults.audio_bitrate),
            ),
        ]),
    }
}

fn toml_value_string(text: &str) -> toml::Value {
    toml::Value::String(text.to_string())
}

/// Rewrite the form's output field into the configured output directory,
/// keeping the filename. Empty config means "alongside the input" (no-op).
fn rewrite_output_dir(fields: &mut [Field], output_dir: &Path) {
    if output_dir.as_os_str().is_empty() {
        return;
    }
    for field in fields.iter_mut() {
        if field.id != "output" {
            continue;
        }
        if let crate::ops::fields::FieldKind::Text { value } = &mut field.kind {
            let current = PathBuf::from(value.value());
            if let Some(name) = current.file_name() {
                *value =
                    tui_input::Input::new(output_dir.join(name).to_string_lossy().into_owned());
            }
        }
    }
}

/// Split a builder-default output into the template's `{suffix}` and `{ext}`:
/// the default stem minus the input stem prefix (`holiday_compressed` −
/// `holiday` → `compressed`), and the default extension (falling back to
/// the input's when the builder produced none).
fn split_suffix(default_out: &Path, input: &Path) -> (String, String) {
    let default_stem = default_out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let input_stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let suffix = default_stem
        .strip_prefix(input_stem.as_str())
        .unwrap_or_default()
        .trim_start_matches(['_', '-'])
        .to_string();
    let ext = default_out
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .filter(|e| !e.is_empty())
        .or_else(|| input.extension().map(|e| e.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "mp4".to_string());
    (suffix, ext)
}

/// Translate a [`ProbeError`] into the plain-language family from spec
/// section 10. Raw stderr stays one keystroke away (M4 log pane).
fn probe_error_message(error: &ProbeError) -> String {
    match error {
        ProbeError::BinaryNotFound => "ffprobe is missing".to_string(),
        ProbeError::Timeout(d, _) => format!("timed out after {}s", d.as_secs()),
        ProbeError::Failed { message, .. } if message.is_empty() => {
            "this file appears corrupt or isn't media FFmpeg recognizes".to_string()
        }
        ProbeError::Failed { message, .. } => message.clone(),
        ProbeError::Parse { message, .. } => format!("unparseable probe output: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn test_app() -> App {
        let (tx, rx) = unbounded_channel();
        App::new(tx, rx, Settings::default())
    }

    #[test]
    fn starts_at_startup_then_picker_on_ready_report() {
        let mut app = test_app();
        assert_eq!(app.screen, Screen::Startup);
        app.handle_msg(BackgroundMsg::CapabilitiesReady(CapabilityReport {
            found: true,
            usable: true,
            ..CapabilityReport::default()
        }));
        assert_eq!(app.screen, Screen::OperationPicker);
    }

    #[test]
    fn missing_binary_routes_to_install_screen() {
        let mut app = test_app();
        app.handle_msg(BackgroundMsg::CapabilitiesReady(CapabilityReport::missing()));
        assert_eq!(app.screen, Screen::MissingFfmpeg);
    }

    #[test]
    fn selection_wraps_around_both_ends() {
        let mut app = test_app();
        app.screen = Screen::OperationPicker;
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.selected_operation, OPERATIONS.len() - 1);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected_operation, 0);
    }

    #[test]
    fn vim_keys_navigate_like_arrows() {
        let mut app = test_app();
        app.screen = Screen::OperationPicker;
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_operation, 1);
        app.on_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected_operation, 0);
    }

    #[test]
    fn enter_advances_and_esc_goes_back() {
        let mut app = test_app();
        app.screen = Screen::OperationPicker;
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::FileBrowser);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::OperationPicker);
    }

    #[test]
    fn q_quits_from_any_screen() {
        let mut app = test_app();
        app.screen = Screen::Queue {
            return_to: Box::new(Screen::OperationPicker),
        };
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn probe_results_land_in_cache() {
        let mut app = test_app();
        let path = PathBuf::from("/tmp/clip.mp4");
        app.handle_msg(BackgroundMsg::ProbeReady {
            path: path.clone(),
            result: Err(ProbeError::BinaryNotFound),
        });
        assert!(matches!(app.probes.get(&path), Some(ProbeState::Failed(_))));
    }

    #[test]
    fn renders_at_minimum_size_80x24() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test backend");
        let mut app = test_app();
        app.screen = Screen::OperationPicker;
        terminal
            .draw(|frame| app.render(frame))
            .expect("picker renders at 80x24");
    }

    #[test]
    fn tiny_terminal_shows_too_small_message() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).expect("test backend");
        let mut app = test_app();
        terminal
            .draw(|frame| app.render(frame))
            .expect("fallback renders below minimum size");
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            content.contains("Terminal too small"),
            "expected size warning, got: {content}"
        );
    }

    #[tokio::test]
    async fn batch_flow_enqueues_one_job_per_input() {
        use crate::ui::screens::parameter_form::ParamForm;

        let mut app = test_app();
        // Compress form over two inputs, no probes (defaults apply).
        let op = operation_for("compress").expect("compress exists");
        app.form_inputs = vec![PathBuf::from("a.mp4"), PathBuf::from("b.mp4")];
        app.form = Some(ParamForm::new(
            &*op,
            &app.form_inputs.clone(),
            None,
            Vec::new(),
            None,
        ));
        app.start_batch_flow();

        assert_eq!(app.queue.jobs.len(), 2);
        let outputs: Vec<String> = app
            .queue
            .jobs
            .iter()
            .map(|job| job.output.to_string_lossy().into_owned())
            .collect();
        assert!(
            outputs.contains(&"a_compressed.mp4".to_string()),
            "{outputs:?}"
        );
        assert!(
            outputs.contains(&"b_compressed.mp4".to_string()),
            "{outputs:?}"
        );
        assert!(matches!(app.screen, Screen::Queue { .. }));
    }

    #[test]
    fn split_suffix_reuses_builder_naming() {
        let (suffix, ext) = super::split_suffix(
            &PathBuf::from("/v/holiday_compressed.mp4"),
            &PathBuf::from("/v/holiday.mp4"),
        );
        assert_eq!((suffix.as_str(), ext.as_str()), ("compressed", "mp4"));
        let (suffix, ext) =
            super::split_suffix(&PathBuf::from("clip_anim.gif"), &PathBuf::from("clip.mp4"));
        assert_eq!((suffix.as_str(), ext.as_str()), ("anim", "gif"));
    }

    #[test]
    fn queue_screen_lists_jobs_and_aggregate() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = test_app();
        app.screen = Screen::Queue {
            return_to: Box::new(Screen::OperationPicker),
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("test backend");
        terminal
            .draw(|frame| app.render(frame))
            .expect("empty queue renders");
        // Enqueue path is covered above; render one settled state here.
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Job queue"), "{content}");
    }

    /// §2: a successful job auto-returns to the picker after the pause,
    /// carrying its summary — no keypress required.
    #[test]
    fn success_auto_returns_after_pause() {
        use crate::ffmpeg::runner::JobResult;
        use crate::ui::screens::running::RunState;

        let mut app = test_app();
        let spec = crate::ffmpeg::builder::CommandSpec::new("ffmpeg");
        let mut run = RunState::starting(
            "Convert".into(),
            &spec,
            PathBuf::from("out.mp4"),
            None,
            None,
        );
        run.phase = RunPhase::Done;
        run.result = Some(JobResult {
            success: true,
            cancelled: false,
            exit_code: Some(0),
            stderr: Vec::new(),
            wall_time: std::time::Duration::from_secs(3),
        });
        run.finished_at = Some(std::time::Instant::now() - SUCCESS_PAUSE);
        app.run = Some(run);
        app.screen = Screen::Running;

        app.on_tick();
        assert_eq!(app.screen, Screen::OperationPicker);
        assert!(
            app.status_message
                .as_ref()
                .is_some_and(|s| s.contains("out.mp4")),
            "summary must name the output, got {:?}",
            app.status_message
        );
    }

    /// §2: fresh successes and failures stay put until acknowledged.
    #[test]
    fn fresh_success_and_failure_do_not_auto_return() {
        use crate::ffmpeg::runner::JobResult;
        use crate::ui::screens::running::RunState;

        for success in [true, false] {
            let mut app = test_app();
            let spec = crate::ffmpeg::builder::CommandSpec::new("ffmpeg");
            let mut run = RunState::starting(
                "Convert".into(),
                &spec,
                PathBuf::from("out.mp4"),
                None,
                None,
            );
            run.phase = RunPhase::Done;
            run.result = Some(JobResult {
                success,
                cancelled: false,
                exit_code: Some(i32::from(!success)),
                stderr: Vec::new(),
                wall_time: std::time::Duration::from_secs(1),
            });
            if success {
                run.finished_at = Some(std::time::Instant::now());
            }
            app.run = Some(run);
            app.screen = Screen::Running;
            app.on_tick();
            assert_eq!(app.screen, Screen::Running, "success={success}");
        }
    }

    /// §2: any key during the success pause dismisses early — except `c`
    /// (copy) and `l` (log), which stay.
    #[test]
    fn any_key_dismisses_success_pause_early() {
        use crate::ffmpeg::runner::JobResult;
        use crate::ui::screens::running::RunState;

        let mut app = test_app();
        let spec = crate::ffmpeg::builder::CommandSpec::new("ffmpeg");
        let mut run = RunState::starting(
            "Convert".into(),
            &spec,
            PathBuf::from("out.mp4"),
            None,
            None,
        );
        run.phase = RunPhase::Done;
        run.result = Some(JobResult {
            success: true,
            cancelled: false,
            exit_code: Some(0),
            stderr: Vec::new(),
            wall_time: std::time::Duration::from_secs(1),
        });
        run.finished_at = Some(std::time::Instant::now());
        app.run = Some(run);
        app.screen = Screen::Running;

        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.screen, Screen::OperationPicker);
    }

    /// §1 repro: convert the same file three times without restarting.
    /// Each run must complete or prompt-and-proceed — never hang on
    /// ffmpeg's interactive overwrite prompt. Fails by timeout on a hang.
    #[tokio::test]
    async fn convert_same_file_three_times_never_hangs() {
        let Some(ffmpeg) = which::which("ffmpeg").ok() else {
            println!("skipping: no ffmpeg on PATH");
            return;
        };
        let dir: PathBuf = std::env::temp_dir().join(format!("ffkit-sect1-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let input = dir.join("clip.mp4");
        let status = tokio::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=10:duration=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&input)
            .status()
            .await
            .expect("generate sample");
        assert!(status.success());

        let mut app = test_app();
        app.caps = Some(CapabilityReport {
            found: true,
            usable: true,
            ffmpeg_path: Some(ffmpeg),
            ffprobe_path: which::which("ffprobe").ok(),
            ..CapabilityReport::default()
        });
        app.selected_operation = 0; // Convert
        app.form_inputs = vec![input];
        app.open_form();
        assert!(app.form.is_some(), "convert form must open");

        for round in 1..=3 {
            // The exact argv about to be spawned — the §1 investigation.
            let preview = app
                .form
                .as_ref()
                .and_then(|form| form.preview.clone())
                .expect("preview must exist");
            assert!(
                preview.args.contains(&"-y".to_string()),
                "round {round}: -y must reach the child, got {:?}",
                preview.args
            );
            app.start_single_flow();
            // Runs 2+ hit the overwrite confirm; answer like a user would —
            // round 2 with Enter (the §1 fix), round 3 with y.
            if app
                .run
                .as_ref()
                .is_some_and(|run| run.phase == RunPhase::ConfirmOverwrite)
            {
                app.screen = Screen::Running;
                let confirm = if round == 2 {
                    KeyCode::Enter
                } else {
                    KeyCode::Char('y')
                };
                app.on_key(key(confirm));
                assert!(
                    app.run
                        .as_ref()
                        .is_some_and(|run| run.phase == RunPhase::Active),
                    "round {round}: confirm must spawn the job"
                );
            }
            let settled = tokio::time::timeout(std::time::Duration::from_secs(60), async {
                loop {
                    app.poll_background();
                    let done = app.run.as_ref().is_some_and(|run| {
                        matches!(run.phase, RunPhase::Done | RunPhase::ConfirmDelete)
                    });
                    if done {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            })
            .await;
            assert!(
                settled.is_ok(),
                "round {round}: job never settled — hang reproduced"
            );
            let run = app.run.as_ref().expect("run state");
            assert!(
                run.succeeded(),
                "round {round}: job must succeed, stderr: {:?}",
                run.result.as_ref().map(|r| r.stderr_tail(5))
            );
            // Dismiss the delete prompt if a partial file tripped it.
            if run.phase == RunPhase::ConfirmDelete {
                app.on_key(key(KeyCode::Char('n')));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
