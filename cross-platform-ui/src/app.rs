use crate::commands_model::CommandsModel;
use crate::console_model::ConsoleModel;
use crate::debug_model::DebugModel;
use crate::engine_client::EngineClient;
use crate::logging;
use crate::metadata_model::MetadataAssembler;
use crate::platform;
use crate::protocol::{self, EngineRecord};
use crate::settings::Settings;
use crate::theme::{self, Palette};
use eframe::egui::{self, Color32, Key, RichText, ScrollArea, TextEdit};

pub struct Ps7IsePrototypeApp {
    console: ConsoleModel,
    engine: Option<EngineClient>,
    palette: Palette,
    script_text: String,
    script_path: Option<std::path::PathBuf>,
    show_script_pane: bool,
    show_commands_pane: bool,
    auto_scroll: bool,
    console_plain_fallback: bool,
    last_title: String,
    metadata: MetadataAssembler,
    commands: CommandsModel,
    debug: DebugModel,
    show_debug_pane: bool,
    settings: Settings,
    status_msg: String,
}

impl Ps7IsePrototypeApp {
    pub fn new() -> Self {
        let settings = Settings::load();
        let mut app = Self {
            console: ConsoleModel::new(),
            engine: None,
            palette: Palette::default(),
            script_text: "# Cross-platform PS7 ISE prototype script pane\nWrite-Host \"Hello from prototype\"\n".to_string(),
            script_path: None,
            show_script_pane: settings.show_script_pane,
            show_commands_pane: false,
            auto_scroll: true,
            console_plain_fallback: false,
            last_title: format!("PS7 ISE Prototype ({})", platform::platform_name()),
            metadata: MetadataAssembler::default(),
            commands: CommandsModel::default(),
            debug: DebugModel::default(),
            show_debug_pane: false,
            settings,
            status_msg: String::new(),
        };
        app.start_engine();
        app
    }

    fn start_engine(&mut self) {
        if let Ok(path) = std::env::var("PS7ISE_ENGINE") {
            let p = std::path::PathBuf::from(&path);
            if p.exists() {
                let mut cmd = std::process::Command::new(&p);
                cmd.current_dir(p.parent().unwrap_or_else(|| std::path::Path::new(".")));
                match EngineClient::spawn_process(cmd, format!("process: {}", p.display())) {
                    Ok(client) => {
                        self.console.status = format!("Engine: {}", client.name);
                        logging::log(&format!("started engine: {}", p.display()));
                        self.engine = Some(client);
                        return;
                    }
                    Err(e) => {
                        self.console.push_system(&format!("Failed to start real engine {path}: {e}\nFalling back to mock.\n"));
                    }
                }
            }
        }

        let repo_root = std::env::current_dir().ok().and_then(|cwd| {
            if cwd.ends_with("cross-platform-ui") { cwd.parent().map(|p| p.to_path_buf()) } else { Some(cwd) }
        });
        for cand in platform::engine_candidate_paths(repo_root.as_deref()) {
            if cand.exists() {
                let mut cmd = std::process::Command::new(&cand);
                cmd.current_dir(cand.parent().unwrap_or_else(|| std::path::Path::new(".")));
                match EngineClient::spawn_process(cmd, format!("process: {}", cand.display())) {
                    Ok(client) => {
                        self.console.push_system(&format!("Connected to engine: {}\n", cand.display()));
                        self.console.status = format!("Engine: {}", client.name);
                        logging::log(&format!("connected engine: {}", cand.display()));
                        self.engine = Some(client);
                        return;
                    }
                    Err(e) => {
                        self.console.push_system(&format!("Found engine candidate {} but failed to start: {e}\n", cand.display()));
                    }
                }
            }
        }

        let client = EngineClient::spawn_mock();
        self.console.status = format!("Engine: {} (pwsh-backed mock)", client.name);
        self.console.push_system("No real PowerShellEngineHost found; using mock protocol engine.\n");
        self.console.push_system("Set PS7ISE_ENGINE=/path/to/PowerShellEngineHost to use a real engine.\n\n");
        logging::log("using mock engine");
        self.engine = Some(client);
    }

    fn drain_engine(&mut self) {
        let mut records = Vec::new();
        if let Some(engine) = &self.engine {
            while let Some(rec) = engine.try_recv() {
                records.push(rec);
            }
        }
        for rec in records {
            match rec {
                EngineRecord::Output(ev) => {
                    if ev.text == "\u{000C}" { self.console.clear(); }
                    else {
                        self.console.push_event(ev.clone());
                        if ev.kind == protocol::EventKind::Title { self.last_title = ev.text.clone(); }
                    }
                }
                EngineRecord::Completions(items) => {
                    self.console.completions = items;
                    self.console.completion_index = None;
                    if !self.console.completions.is_empty() { self.console.next_completion(); }
                }
                EngineRecord::Metadata(chunk) => {
                    if let Some(reply) = self.metadata.feed(chunk) {
                        if reply.kind == "i" { self.commands.load_index_payload(&reply.payload); self.status_msg = format!("Loaded {} commands", self.commands.commands.len()); }
                        else if reply.kind == "d" { self.commands.detail = reply.payload; }
                        else if reply.kind == "b" { self.debug.load_breakpoints(&reply.payload); }
                        else if reply.kind == "k" { self.debug.load_call_stack(&reply.payload); }
                    }
                }
                EngineRecord::DebugStop(stop) => {
                    self.console.busy = false;
                    self.console.status = format!("Debug stop: {} {}:{}:{}", stop.reason, stop.path, stop.line, stop.col);
                    let msg = format!("[debug stop] {} at {}:{}:{}\n", stop.reason, stop.path, stop.line, stop.col);
                    self.console.push_system(&msg);
                    self.debug.last_stop = Some(msg);
                    self.show_debug_pane = true;
                    if let Some(engine) = &mut self.engine {
                        let _ = engine.send(&protocol::cmd_debug_stack());
                        let _ = engine.send(&protocol::cmd_debug_breakpoints());
                    }
                }
                EngineRecord::DebugResumed => {
                    self.console.status = "Debug resumed".to_string();
                    self.console.push_system("[debug resumed]\n");
                    self.debug.last_stop = None;
                }
                EngineRecord::Quit(code) => {
                    self.console.exit_code = Some(code);
                    self.console.busy = false;
                    self.console.input_requested = false;
                    self.console.status = format!("Engine exited ({code})");
                    self.console.push_system(&format!("\n[engine exited with code {code}]\n"));
                }
                EngineRecord::Foreign(line) => {
                    if !line.is_empty() { self.console.push_system(&format!("[foreign] {line}\n")); }
                }
            }
        }
    }

    fn send_line(&mut self) {
        let line = self.console.input.clone();
        if !line.trim().is_empty() { self.console.history.push(line.clone()); }
        self.console.history_index = None;
        self.console.completions.clear();
        self.console.completion_index = None;
        self.console.push_text(&format!("{line}\n"), self.palette.fg);
        self.console.input.clear();
        if let Some(engine) = &mut self.engine { let _ = engine.send(&protocol::cmd_submit_line(&line)); }
    }

    fn send_stop(&mut self) { if let Some(engine) = &mut self.engine { let _ = engine.send(&protocol::cmd_stop()); } }
    fn request_completion(&mut self) {
        let text = self.console.input.clone();
        let caret = text.chars().count();
        if let Some(engine) = &mut self.engine { let _ = engine.send(&protocol::cmd_complete(caret, &text)); }
    }
    fn run_script(&mut self) {
        let script = self.script_text.clone();
        if let Some(engine) = &mut self.engine { let _ = engine.send(&protocol::cmd_submit_line(&script)); }
        self.console.push_system("[Run Script]\n");
    }
    fn run_selection(&mut self, selection: String) {
        if selection.trim().is_empty() { return; }
        if let Some(engine) = &mut self.engine { let _ = engine.send(&protocol::cmd_submit_line(&selection)); }
        self.console.push_system("[Run Selection]\n");
    }
    fn open_script(&mut self) {
        if let Some(pb) = rfd::FileDialog::new().add_filter("PowerShell", &["ps1","psm1","psd1"]).add_filter("All files", &["*"]).pick_file() {
            match std::fs::read_to_string(&pb) {
                Ok(content) => {
                    self.script_text = content;
                    self.script_path = Some(pb.clone());
                    self.status_msg = format!("Opened {}", pb.display());
                    logging::log(&format!("opened script {}", pb.display()));
                }
                Err(e) => { self.status_msg = format!("Open failed: {e}"); }
            }
        } else if let Ok(p) = std::env::var("PS7ISE_OPEN") {
            let pb = std::path::PathBuf::from(p);
            if let Ok(content) = std::fs::read_to_string(&pb) {
                self.script_text = content; self.script_path = Some(pb.clone());
                self.status_msg = format!("Opened {}", pb.display());
            }
        }
    }
    fn save_script(&mut self) {
        let path = self.script_path.clone().or_else(|| {
            rfd::FileDialog::new().add_filter("PowerShell", &["ps1"]).set_file_name("script.ps1").save_file()
        });
        if let Some(pb) = path {
            match std::fs::write(&pb, &self.script_text) {
                Ok(_) => { self.script_path = Some(pb.clone()); self.status_msg = format!("Saved {}", pb.display()); logging::log(&format!("saved {}", pb.display())); self.settings.save(); }
                Err(e) => { self.status_msg = format!("Save failed: {e}"); }
            }
        } else if let Ok(p) = std::env::var("PS7ISE_SAVE") {
            let pb = std::path::PathBuf::from(p);
            match std::fs::write(&pb, &self.script_text) {
                Ok(_) => { self.script_path = Some(pb.clone()); self.status_msg = format!("Saved {}", pb.display()); }
                Err(e) => { self.status_msg = format!("Save failed: {e}"); }
            }
        }
    }
    fn request_command_index(&mut self) {
        if let Some(engine) = &mut self.engine { let _ = engine.send(&protocol::cmd_meta_index()); self.status_msg = "Requested command index...".to_string(); }
    }

    fn debug_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Continue").clicked() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_continue()); } }
            if ui.button("Step Over").clicked() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_step_over()); } }
            if ui.button("Step Into").clicked() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_step_into()); } }
            if ui.button("Step Out").clicked() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_step_out()); } }
            if ui.button("Stop").clicked() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_stop()); } }
            if ui.button("Refresh").clicked() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_breakpoints()); let _=e.send(&protocol::cmd_debug_stack()); } }
        });
        if let Some(stop) = &self.debug.last_stop {
            ui.label(RichText::new(stop.trim()).color(Color32::YELLOW));
        }
        ui.separator();
        ui.label(RichText::new("Breakpoints").strong());
        ui.horizontal(|ui| {
            ui.label("Line"); ui.add(TextEdit::singleline(&mut self.debug.new_bp_line).desired_width(60.0));
            ui.label("Path"); ui.add(TextEdit::singleline(&mut self.debug.new_bp_path).desired_width(180.0));
            if ui.button("Add").clicked() {
                if let Ok(line) = self.debug.new_bp_line.trim().parse::<i64>() {
                    let path = self.debug.new_bp_path.trim().to_string();
                    if !path.is_empty() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_breakpoint_add(line, &path)); let _=e.send(&protocol::cmd_debug_breakpoints()); } }
                }
            }
            if ui.button("Clear All").clicked() { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_breakpoint_clear()); let _=e.send(&protocol::cmd_debug_breakpoints()); } }
        });
        let bps: Vec<(i64, String, i64, bool, i64)> = self.debug.breakpoints.iter().map(|b| (b.id, b.script.clone(), b.line, b.enabled, b.hit_count)).collect();
        let mut remove_id: Option<i64> = None;
        let mut toggle_id: Option<(i64,bool)> = None;
        ScrollArea::vertical().id_source("bps").max_height(120.0).show(ui, |ui| {
            for (id, script, line, enabled, hits) in &bps {
                ui.horizontal(|ui| {
                    ui.label(format!("#{} {}:{} {} hits={}", id, script, line, if *enabled {"on"} else {"off"}, hits));
                    let tlabel = if *enabled { "Disable" } else { "Enable" };
                    if ui.small_button(tlabel).clicked() { toggle_id = Some((*id, *enabled)); }
                    if ui.small_button("Remove").clicked() { remove_id = Some(*id); }
                });
            }
        });
        if let Some((id, was_enabled)) = toggle_id {
            if let Some(e)=&mut self.engine {
                let cmd = if was_enabled { protocol::cmd_debug_breakpoint_disable(id) } else { protocol::cmd_debug_breakpoint_enable(id) };
                let _=e.send(&cmd);
                let _=e.send(&protocol::cmd_debug_breakpoints());
            }
        }
        if let Some(id) = remove_id { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_breakpoint_remove(id)); let _=e.send(&protocol::cmd_debug_breakpoints()); } }
        ui.separator();
        ui.label(RichText::new("Call Stack").strong());
        let stack: Vec<(i64,String,String,i64)> = self.debug.call_stack.iter().map(|f| (f.index, f.function.clone(), f.script.clone(), f.line)).collect();
        ScrollArea::vertical().id_source("stack").max_height(120.0).show(ui, |ui| {
            for (_i, func, script, line) in &stack {
                ui.label(format!("{}  {}:{}", func, script, line));
            }
        });
    }

    fn console_ui(&mut self, ui: &mut egui::Ui) {
        let bg = theme::color32(self.palette.bg);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.auto_scroll, "Auto-scroll");
            ui.checkbox(&mut self.console_plain_fallback, "Plain text fallback");
            if ui.button("Copy All").clicked() {
                ui.output_mut(|o| o.copied_text = self.console.plain.clone());
                self.status_msg = "Console copied to clipboard".to_string();
            }
        });
        let frame = egui::Frame::none().fill(bg).inner_margin(8.0);
        frame.show(ui, |ui| {
            ScrollArea::vertical().auto_shrink([false; 2]).stick_to_bottom(self.auto_scroll).show(ui, |ui| {
                if self.console_plain_fallback {
                    let mut text = if self.console.plain.is_empty() { "(console ready)\n".to_string() } else { self.console.plain.clone() };
                    ui.add(
                        TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(20)
                            .interactive(true)
                            .frame(false)
                            .text_color(theme::color32(self.palette.fg))
                    );
                } else {
                    let mut job = egui::text::LayoutJob::default();
                    for run in &self.console.runs {
                        job.append(&run.text, 0.0, egui::TextFormat {
                            font_id: egui::FontId::monospace(self.settings.font_size),
                            color: theme::color32(run.fg),
                            ..Default::default()
                        });
                    }
                    if job.text.is_empty() {
                        job.append("(console ready)\n", 0.0, egui::TextFormat {
                            font_id: egui::FontId::monospace(self.settings.font_size),
                            color: Color32::GRAY, ..Default::default()
                        });
                    }
                    ui.add(egui::Label::new(job).selectable(true));
                }
            });
        });
    }

    fn commands_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.text_edit_singleline(&mut self.commands.filter);
            if ui.button("Refresh").clicked() { self.request_command_index(); }
        });
        ui.separator();
        let filtered: Vec<(usize, String, String, String)> = self.commands.filtered()
            .into_iter().take(300)
            .map(|(idx, cmd)| (idx, cmd.name.clone(), cmd.module.clone(), cmd.raw.clone()))
            .collect();
        let selected_idx = self.commands.selected;
        let mut new_selected: Option<(usize, String)> = None;
        ScrollArea::vertical().id_source("commands").max_height(300.0).show(ui, |ui| {
            for (idx, name, module, raw) in &filtered {
                let selected = selected_idx == Some(*idx);
                let label = format!("{}  [{}]", name, module);
                if ui.selectable_label(selected, label).clicked() {
                    new_selected = Some((*idx, raw.clone()));
                }
            }
        });
        if let Some((idx, raw)) = new_selected {
            self.commands.selected = Some(idx);
            self.commands.detail = raw.clone();
            if let Some(name) = self.commands.commands.get(idx).map(|c| c.name.clone()) {
                if let Some(engine) = &mut self.engine {
                    let _ = engine.send(&protocol::cmd_meta_detail(&name));
                }
            }
        }
        ui.separator();
        ui.label(RichText::new("Detail").strong());
        ScrollArea::vertical().id_source("cmd-detail").max_height(120.0).show(ui, |ui| {
            ui.add(TextEdit::multiline(&mut self.commands.detail).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
        });
        ui.horizontal(|ui| {
            if ui.button("Insert selected name").clicked() {
                if let Some(i) = self.commands.selected {
                    if let Some(cmd) = self.commands.commands.get(i) {
                        self.console.input.push_str(&cmd.name);
                    }
                }
            }
            if ui.button("Run selected").clicked() {
                if let Some(i) = self.commands.selected {
                    if let Some(cmd) = self.commands.commands.get(i).cloned() {
                        self.console.input = cmd.name.clone();
                        self.send_line();
                    }
                }
            }
        });
    }
}

impl eframe::App for Ps7IsePrototypeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_engine();
        ctx.request_repaint_after(std::time::Duration::from_millis(50));

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("PS7 ISE Prototype");
                ui.separator();
                if ui.button("Run Script (F5)").clicked() { self.run_script(); }
                if ui.button("Run Selection").clicked() { /* selection capture is limited in basic egui text edit; fallback below */ self.status_msg = "Use Ctrl+Enter in script pane to run selection (prototype)".into(); }
                if ui.button("Stop (Ctrl+C)").clicked() { self.send_stop(); }
                if ui.button("Clear").clicked() { self.console.clear(); let _ = self.engine.as_mut().map(|e| e.send(&protocol::cmd_prompt())); }
                if ui.button("Open").clicked() { self.open_script(); }
                if ui.button("Save").clicked() { self.save_script(); }
                if ui.button("Commands").clicked() { self.show_commands_pane = !self.show_commands_pane; if self.show_commands_pane { self.request_command_index(); } }
                if ui.button("Debug").clicked() { self.show_debug_pane = !self.show_debug_pane; if self.show_debug_pane { if let Some(e)=&mut self.engine { let _=e.send(&protocol::cmd_debug_breakpoints()); let _=e.send(&protocol::cmd_debug_stack()); } } }
                if ui.button("Toggle Script Pane").clicked() { self.show_script_pane = !self.show_script_pane; self.settings.show_script_pane = self.show_script_pane; self.settings.save(); }
                ui.separator();
                ui.label(RichText::new(&self.console.status).color(Color32::LIGHT_GREEN));
                if self.console.busy { ui.label(RichText::new("BUSY").color(Color32::YELLOW)); }
            });
            if !self.status_msg.is_empty() { ui.label(RichText::new(&self.status_msg).small().color(Color32::LIGHT_BLUE)); }
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Platform: {}", platform::platform_name()));
                ui.separator();
                ui.label(format!("Engine: {}", self.engine.as_ref().map(|e| e.name.as_str()).unwrap_or("none")));
                ui.separator();
                if let Some(p) = &self.script_path { ui.label(format!("Script: {}", p.display())); }
                ui.separator();
                ui.label("OS-agnostic prototype - protocol/core split");
            });
        });

        egui::TopBottomPanel::bottom("input").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("PS>").monospace().color(theme::color32(self.palette.prompt)));
                let resp = ui.add(TextEdit::singleline(&mut self.console.input).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).lock_focus(true));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) { self.send_line(); resp.request_focus(); }
                if resp.has_focus() {
                    if ui.input(|i| i.key_pressed(Key::ArrowUp)) { self.console.history_up(); }
                    if ui.input(|i| i.key_pressed(Key::ArrowDown)) { self.console.history_down(); }
                    if ui.input(|i| i.key_pressed(Key::Tab)) { self.request_completion(); }
                    if ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::C)) { self.send_stop(); }
                    if ui.input(|i| i.key_pressed(Key::F5)) { self.run_script(); }
                }
            });
            if !self.console.completions.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Completions:");
                    for (i, c) in self.console.completions.iter().take(12).enumerate() {
                        let selected = self.console.completion_index == Some(i);
                        let text = if selected { RichText::new(c).strong().color(Color32::YELLOW) } else { RichText::new(c) };
                        if ui.button(text).clicked() { self.console.input = c.clone(); self.console.completion_index = Some(i); }
                    }
                });
            }
        });

        egui::SidePanel::right("commands").resizable(true).default_width(320.0).show_animated(ctx, self.show_commands_pane, |ui| {
            ui.heading("Commands");
            self.commands_ui(ui);
        });

        egui::SidePanel::left("debug").resizable(true).default_width(360.0).show_animated(ctx, self.show_debug_pane, |ui| {
            ui.heading("Debug");
            self.debug_ui(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.show_script_pane {
                ui.columns(2, |cols| {
                    cols[0].vertical(|ui| {
                        ui.horizontal(|ui| { ui.label(RichText::new("Script Pane").strong()); if ui.small_button("Run").clicked() { self.run_script(); } });
                        let script_resp = ScrollArea::vertical().id_source("script").show(ui, |ui| {
                            ui.add(TextEdit::multiline(&mut self.script_text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).desired_rows(20))
                        }).inner;
                        if script_resp.has_focus() && ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Enter)) {
                            // no easy selection API from basic TextEdit; run whole script as fallback
                            self.run_script();
                        }
                        if script_resp.has_focus() && ui.input(|i| i.key_pressed(Key::F5)) { self.run_script(); }
                    });
                    cols[1].vertical(|ui| {
                        ui.label(RichText::new("Console").strong());
                        self.console_ui(ui);
                    });
                });
            } else {
                ui.label(RichText::new("Console").strong());
                self.console_ui(ui);
            }
        });
    }
}
