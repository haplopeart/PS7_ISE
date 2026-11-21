use crate::protocol::{self, EngineRecord};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

pub struct EngineClient {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    mock_tx: Option<Sender<String>>,
    rx: Receiver<EngineRecord>,
    pub name: String,
}

impl EngineClient {
    pub fn spawn_process(mut cmd: Command, name: String) -> Result<Self, String> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| format!("Failed to start engine: {e}"))?;
        let stdin = child.stdin.take().ok_or_else(|| "No engine stdin".to_string())?;
        let stdout = child.stdout.take().ok_or_else(|| "No engine stdout".to_string())?;
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("engine-reader".to_string())
            .spawn(move || read_stdout(stdout, tx))
            .map_err(|e| format!("reader thread: {e}"))?;
        Ok(Self { child: Some(child), stdin: Some(stdin), mock_tx: None, rx, name })
    }

    pub fn spawn_mock() -> Self {
        let (tx_out, rx) = mpsc::channel::<EngineRecord>();
        let (tx_in, rx_in) = mpsc::channel::<String>();
        thread::Builder::new()
            .name("mock-engine".to_string())
            .spawn(move || mock_engine_loop(rx_in, tx_out))
            .expect("mock engine thread");
        Self { child: None, stdin: None, mock_tx: Some(tx_in), rx, name: "mock-pwsh".to_string() }
    }

    pub fn send(&mut self, cmd: &str) -> Result<(), String> {
        if let Some(tx) = &self.mock_tx {
            tx.send(cmd.to_string()).map_err(|e| format!("mock send: {e}"))?;
            return Ok(());
        }
        let stdin = self.stdin.as_mut().ok_or_else(|| "engine stdin closed".to_string())?;
        stdin.write_all(cmd.as_bytes()).map_err(|e| format!("engine write: {e}"))?;
        stdin.flush().map_err(|e| format!("engine flush: {e}"))?;
        Ok(())
    }

    pub fn try_recv(&self) -> Option<EngineRecord> {
        self.rx.try_recv().ok()
    }

    pub fn is_mock(&self) -> bool { self.mock_tx.is_some() }

    pub fn shutdown(&mut self) {
        let _ = self.send(&protocol::cmd_exit());
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        self.mock_tx.take();
    }
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn read_stdout(stdout: std::process::ChildStdout, tx: Sender<EngineRecord>) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let rec = protocol::parse_record(&line);
        if tx.send(rec).is_err() { break; }
    }
    let _ = tx.send(EngineRecord::Quit(-1));
}

fn mock_engine_loop(rx: Receiver<String>, tx: Sender<EngineRecord>) {
    let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
        kind: protocol::EventKind::Output,
        fg: [204, 204, 204], bg: [0, 0, 0],
        text: "PS7 ISE Cross-Platform Prototype (mock engine)\n".to_string(),
    }));
    let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
        kind: protocol::EventKind::Output,
        fg: [150, 150, 150], bg: [0, 0, 0],
        text: "Protocol-compatible UI prototype. Real PowerShellEngineHost can be plugged in later.\n\n".to_string(),
    }));
    emit_prompt(&tx);

    while let Ok(cmd) = rx.recv() {
        let cmd = cmd.trim_end_matches(['\n', '\r']).to_string();
        if cmd.is_empty() { continue; }
        let verb = cmd.chars().next().unwrap_or(' ');
        let arg = if cmd.len() > 2 { cmd[2..].to_string() } else { String::new() };
        match verb {
            'L' => {
                let line = protocol::decode(&arg);
                let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
                    kind: protocol::EventKind::CommandStarted, fg: [204,204,204], bg: [0,0,0], text: String::new(),
                }));
                let should_quit = run_mock_command(&line, &tx);
                let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
                    kind: protocol::EventKind::CommandFinished, fg: [204,204,204], bg: [0,0,0], text: String::new(),
                }));
                if should_quit { break; }
                emit_prompt(&tx);
            }
            'T' => {
                let mut parts = arg.splitn(2, ' ');
                let _caret = parts.next().unwrap_or("0");
                let text = protocol::decode(parts.next().unwrap_or(""));
                let completions = mock_completions(&text);
                let _ = tx.send(EngineRecord::Completions(completions));
            }
            'S' => {
                let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
                    kind: protocol::EventKind::Error, fg: [255,100,100], bg: [0,0,0],
                    text: "Command cancelled.\n".to_string(),
                }));
                emit_prompt(&tx);
            }
            'P' => emit_prompt(&tx),
            'X' => { let _ = tx.send(EngineRecord::Quit(0)); break; }
            _ => {}
        }
    }
}

fn emit_prompt(tx: &Sender<EngineRecord>) {
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| "~".to_string());
    let prompt = format!("PS {}> ", cwd);
    let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
        kind: protocol::EventKind::Output, fg: [100, 200, 100], bg: [0,0,0], text: prompt,
    }));
    let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
        kind: protocol::EventKind::InputRequested, fg: [204,204,204], bg: [0,0,0], text: String::new(),
    }));
}

fn run_mock_command(line: &str, tx: &Sender<EngineRecord>) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() { return false; }
    if trimmed.eq_ignore_ascii_case("exit") {
        let _ = tx.send(EngineRecord::Quit(0));
        return true;
    }
    if trimmed.eq_ignore_ascii_case("clear") || trimmed.eq_ignore_ascii_case("cls") || trimmed.eq_ignore_ascii_case("clear-host") {
        let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
            kind: protocol::EventKind::Output, fg: [204,204,204], bg: [0,0,0], text: "\u{000C}".to_string(),
        }));
        return false;
    }

    let pwsh = which_pwsh();
    if let Some(pwsh) = pwsh {
        let out = Command::new(&pwsh)
            .args(["-NoProfile", "-NonInteractive", "-Command", trimmed])
            .output();
        match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                if !stdout.is_empty() {
                    let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
                        kind: protocol::EventKind::Output, fg: [204,204,204], bg: [0,0,0], text: ensure_newline(&stdout),
                    }));
                }
                if !stderr.is_empty() {
                    let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
                        kind: protocol::EventKind::Error, fg: [255,100,100], bg: [0,0,0], text: ensure_newline(&stderr),
                    }));
                }
                if !o.status.success() && stdout.is_empty() && stderr.is_empty() {
                    let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
                        kind: protocol::EventKind::Error, fg: [255,100,100], bg: [0,0,0],
                        text: format!("Command exited with status {}\n", o.status),
                    }));
                }
            }
            Err(e) => {
                let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
                    kind: protocol::EventKind::Error, fg: [255,100,100], bg: [0,0,0],
                    text: format!("Failed to run pwsh: {e}\n"),
                }));
            }
        }
    } else {
        let _ = tx.send(EngineRecord::Output(protocol::OutputEvent {
            kind: protocol::EventKind::Warning, fg: [255,200,100], bg: [0,0,0],
            text: format!("Mock engine: pwsh not found, cannot execute: {trimmed}\n"),
        }));
    }
    false
}

fn ensure_newline(s: &str) -> String {
    if s.ends_with('\n') { s.to_string() } else { format!("{s}\n") }
}

fn which_pwsh() -> Option<PathBuf> {
    for name in ["pwsh", "powershell"] {
        if let Ok(out) = Command::new("/usr/bin/which").arg(name).output() {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() { return Some(PathBuf::from(p)); }
            }
        }
    }
    // Fallback: common locations
    for p in ["/usr/local/bin/pwsh", "/opt/homebrew/bin/pwsh", "/usr/bin/pwsh"] {
        let pb = PathBuf::from(p);
        if pb.exists() { return Some(pb); }
    }
    None
}

fn mock_completions(text: &str) -> Vec<String> {
    let prefix = text.split_whitespace().last().unwrap_or(text);
    let candidates = ["Get-ChildItem","Get-Content","Get-Command","Get-Help","Get-Process","Get-Service",
        "Set-Location","Select-Object","Where-Object","ForEach-Object","Write-Host","Write-Output",
        "Import-Module","Invoke-WebRequest","Invoke-RestMethod","Test-Path","New-Item","Remove-Item",
        "$PSVersionTable","Get-Date","Clear-Host","Exit"];
    candidates.iter().filter(|c| c.to_lowercase().starts_with(&prefix.to_lowercase())).map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{self, EngineRecord, EventKind};
    use std::time::{Duration, Instant};

    fn recv_until<F: Fn(&EngineRecord) -> bool>(client: &EngineClient, pred: F, timeout_ms: u64) -> Option<EngineRecord> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(timeout_ms) {
            if let Some(rec) = client.try_recv() {
                if pred(&rec) { return Some(rec); }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        None
    }

    #[test]
    fn mock_engine_prompt_and_command() {
        let mut client = EngineClient::spawn_mock();
        // Should get banner/prompt/input requested
        let got_prompt = recv_until(&client, |r| matches!(r, EngineRecord::Output(ev) if ev.kind == EventKind::InputRequested), 2000);
        assert!(got_prompt.is_some(), "expected initial prompt/input request");

        client.send(&protocol::cmd_submit_line("Write-Output 'hello-from-mock'")).unwrap();
        let got_output = recv_until(&client, |r| matches!(r, EngineRecord::Output(ev) if ev.text.contains("hello-from-mock")), 5000);
        assert!(got_output.is_some(), "expected command output containing hello-from-mock");

        let got_done = recv_until(&client, |r| matches!(r, EngineRecord::Output(ev) if ev.kind == EventKind::InputRequested), 5000);
        assert!(got_done.is_some(), "expected prompt after command");
    }

    #[test]
    fn mock_engine_completions() {
        let mut client = EngineClient::spawn_mock();
        let _ = recv_until(&client, |r| matches!(r, EngineRecord::Output(ev) if ev.kind == EventKind::InputRequested), 2000);
        client.send(&protocol::cmd_complete(3, "Get")).unwrap();
        let got = recv_until(&client, |r| matches!(r, EngineRecord::Completions(items) if items.iter().any(|i| i.starts_with("Get-"))), 3000);
        assert!(got.is_some(), "expected completions for Get");
    }
}
