use crate::frame::{self, Candidate, Credentials, Network, Outcome, Probe, Scan};
use crate::i18n::{self, t};
use std::{net::Ipv4Addr, sync::atomic::Ordering, sync::mpsc::{self, Receiver}, thread};
use zeroize::Zeroize;
use std::sync::{Mutex, OnceLock};

static FRAME_LOGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
pub(crate) fn take_shared_logs() -> Vec<String> {
    std::mem::take(&mut *FRAME_LOGS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()))
}
fn share_logs(lines: Vec<String>) {
    FRAME_LOGS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).extend(lines);
}

enum Event {
    Probe(Ipv4Addr, String, Result<Probe, String>),
    Log(String),
    Execute(Outcome),
}
pub struct FrameUi {
    scan: Option<Scan>,
    networks: Vec<Network>,
    large_scan: bool,
    candidates: Vec<Candidate>,
    ip: String,
    username: String,
    password: String,
    receiver: Option<Receiver<Event>>,
    preview: Option<(Ipv4Addr, String, Probe)>,
    trust_new: bool,
    status: String,
    status_error: bool,
    error_dialog: Option<String>,
    lines: Vec<String>,
    exported: Vec<String>,
    demo: bool,
    result: Option<frame::SetupResult>,
}
impl Drop for FrameUi {
    fn drop(&mut self) {
        self.password.zeroize();
        if let Some(scan) = &self.scan {
            scan.cancel.store(true, Ordering::Relaxed);
        }
    }
}
impl FrameUi {
    pub fn new(demo: bool) -> Self {
        let mut this = Self {
            scan: None,
            networks: vec![],
            large_scan: false,
            candidates: vec![],
            ip: String::new(),
            username: "steamos".into(),
            password: String::new(),
            receiver: None,
            preview: None,
            trust_new: false,
            status: String::new(),
            status_error: false,
            error_dialog: None,
            lines: vec![],
            exported: vec![],
            demo,
            result: None,
        };
        if demo {
            this.status = t(
                "演示模式：不会连接网络或修改头显。",
                "Demo mode: no network connection or headset changes.",
            )
            .into();
        } else {
            this.scan();
        }
        this
    }
    fn scan(&mut self) {
        if let Some(old) = &self.scan {
            old.cancel.store(true, Ordering::Relaxed);
        }
        self.scan = None;
        self.candidates.clear();
        self.large_scan = false;
        self.status_error = false;
        match frame::networks() {
            Ok(nets) if nets.is_empty() => {
                self.status = t(
                    "未找到活动的局域网 IPv4 网段，请手动输入 Frame IP。",
                    "No active local IPv4 subnet found. Enter the Frame IP manually.",
                )
                .into()
            }
            Ok(nets) => {
                let count: u64 = nets.iter().map(|n| n.host_count()).sum();
                self.networks = nets;
                if count > 4096 {
                    self.large_scan = true;
                    self.status = if i18n::is_english() {
                        format!("This subnet has {count} addresses. Scanning may take a while.")
                    } else {
                        format!("当前网段包含 {count} 个地址，扫描可能较慢。请选择是否继续。")
                    };
                } else {
                    self.start_scan();
                }
            }
            Err(e) => {
                self.error(if i18n::is_english() {
                    format!("Scan unavailable: {e}. Enter the IP manually.")
                } else {
                    format!("扫描不可用：{e}。可手动输入 IP。")
                });
            }
        }
    }
    fn start_scan(&mut self) {
        self.large_scan = false;
        self.status_error = false;
        self.scan = Some(frame::start_scan(self.networks.clone()));
        self.status.clear();
    }
    fn relocalize_status(&mut self) {
        self.status = if self.demo {
            t("演示模式：不会连接网络或修改头显。", "Demo mode: no network connection or headset changes.").into()
        } else if self.scan.is_some() {
            String::new()
        } else if self.large_scan {
            let count: u64 = self.networks.iter().map(|n| n.host_count()).sum();
            if i18n::is_english() { format!("This network has {count} addresses. Scanning may take a while.") }
            else { format!("当前网段包含 {count} 个地址，扫描可能较慢。请选择是否继续。") }
        } else if self.preview.is_some() {
            t("请核对并信任主机指纹。", "Verify and trust the host fingerprint.").into()
        } else if self.receiver.is_some() {
            t("正在连接或执行，请稍候…", "Connecting or applying settings…").into()
        } else if let Some(result) = self.result {
            result.message().into()
        } else { String::new() };
    }
    fn poll(&mut self) {
        if let Some(scan) = &self.scan {
            while let Ok(item) = scan.results.try_recv() {
                if let Some(old) = self.candidates.iter_mut().find(|old| old.ip == item.ip) {
                    old.ssh_open |= item.ssh_open;
                } else {
                    self.candidates.push(item);
                }
            }
            if scan.done.load(Ordering::Acquire) {
                let summary = if scan.cancel.load(Ordering::Relaxed) {
                    t(
                        "扫描已取消，可手动输入 IP。",
                        "Scan cancelled. Enter an IP manually.",
                    )
                    .into()
                } else {
                    String::new()
                };
                if self.receiver.is_none() && self.lines.is_empty() {
                    self.status = summary;
                }
                self.scan = None;
            }
        }
        loop { match self.receiver.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(Event::Probe(ip, username, result))) => {
                self.receiver = None;
                match result {
                    Ok(probe)
                        if self.ip.trim() == ip.to_string() && self.username.trim() == username =>
                    {
                        self.trust_new = !probe.new_host;
                        self.preview = Some((ip, username, probe));
                        self.status_error = false;
                        self.status = t(
                            "请核对并信任主机指纹。",
                            "Verify and trust the host fingerprint.",
                        )
                        .into();
                        if self.trust_new { self.execute(); }
                    }
                    Ok(_) => self.error(
                        t(
                            "连接信息已变化，请重新确认。",
                            "Connection details changed. Please confirm again.",
                        )
                        .into(),
                    ),
                    Err(e) => self.error(e),
                }
            }
            Some(Ok(Event::Log(line))) => {
                self.exported.push(line.clone());
                self.lines.push(line);
            }
            Some(Ok(Event::Execute(outcome))) => {
                self.receiver = None;
                self.password.zeroize();
                self.result = Some(outcome.result);
                self.status = outcome.result.message().into();
                self.status_error = outcome.error.is_some();
                self.error_dialog = Some(match outcome.error {
                    Some(error) => format!("{}\n{error}", self.status),
                    None => self.status.clone(),
                });
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.receiver = None;
                self.error(
                    t(
                        "连接线程意外结束；结果未知，请在头显上检查。",
                        "Connection thread ended unexpectedly. Result unknown; check the headset.",
                    )
                    .into(),
                );
            }
            _ => break,
        } }
    }
    fn error(&mut self, error: String) {
        self.preview = None;
        self.password.zeroize();
        self.result = Some(frame::SetupResult::Failed);
        self.error_dialog = Some(error.clone());
        self.status = if i18n::is_english() {
            "Headset operation failed. See the diagnostic log below.".into()
        } else {
            error.clone()
        };
        self.status_error = true;
        self.lines.push(format!("[ERROR] {error}"));
        self.exported.push(format!("[ERROR] {error}"));
    }
    fn probe(&mut self) {
        if self.busy() { return; }
        self.result = None;
        self.error_dialog = None;
        let Ok(ip) = self.ip.trim().parse::<Ipv4Addr>() else {
            self.error(t("请输入有效的 IPv4 地址。", "Enter a valid IPv4 address.").into());
            return;
        };
        if self.username.trim().is_empty() || self.password.is_empty() {
            self.error(
                t(
                    "请填写用户名和 SSH 密码。",
                    "Enter a username and SSH password.",
                )
                .into(),
            );
            return;
        }
        self.status = t(
            "正在读取 SSH 主机密钥；尚未发送密码…",
            "Reading SSH host key; password not sent yet…",
        )
        .into();
        self.status_error = false;
        let username = self.username.trim().to_owned();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(Event::Probe(ip, username, frame::probe(ip)));
        });
    }
    fn execute(&mut self) {
        let Some((ip, username, preview)) = self.preview.take() else {
            return;
        };
        if self.ip.trim() != ip.to_string() || self.username.trim() != username {
            self.error(
                t(
                    "连接信息已变化，请重新确认。",
                    "Connection details changed. Please confirm again.",
                )
                .into(),
            );
            return;
        }
        let credentials = Credentials {
            username,
            password: std::mem::take(&mut self.password),
            sudo_password: String::new(),
        };
        self.status = t(
            "正在通过 SSH 设置并复查…",
            "Applying settings over SSH and verifying…",
        )
        .into();
        self.status_error = false;
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(Event::Execute(frame::execute(
                ip,
                &preview.fingerprint,
                credentials,
                |line| { let _ = tx.send(Event::Log(line)); },
            )));
        });
    }
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.exported)
    }
    pub fn busy(&self) -> bool {
        self.receiver.is_some() || self.preview.is_some()
    }
}

#[cfg(test)]
mod error_tests {
    use super::{Event, FrameUi, Outcome, mpsc, frame};

    #[test]
    fn every_final_result_opens_a_dialog_and_retains_logs() {
        for result in [frame::SetupResult::Success, frame::SetupResult::NeedsRestart,
            frame::SetupResult::Partial, frame::SetupResult::Failed] {
            let mut ui = FrameUi::new(true);
            let (tx, rx) = mpsc::channel();
            ui.receiver = Some(rx);
            tx.send(Event::Log("execution details".into())).unwrap();
            let failed = matches!(result, frame::SetupResult::Partial | frame::SetupResult::Failed);
            tx.send(Event::Execute(Outcome {
                result, error: failed.then(|| "failure details".into()),
            })).unwrap();
            ui.poll();
            assert!(ui.error_dialog.as_ref().unwrap().contains(result.message()));
            assert_eq!(ui.status_error, failed);
            assert!(ui.lines.iter().any(|line| line == "execution details"));
            assert!(!ui.busy());
            ui.error_dialog = None;
            ui.poll();
            assert!(ui.error_dialog.is_none());
        }
    }

    #[test]
    fn first_connection_waits_for_trust_and_blocks_repeat_clicks() {
        let mut ui = FrameUi::new(true);
        ui.ip = "192.0.2.1".into();
        ui.password = "secret".into();
        let (tx, rx) = mpsc::channel();
        ui.receiver = Some(rx);
        tx.send(Event::Probe(ui.ip.parse().unwrap(), "steamos".into(), Ok(frame::Probe {
            fingerprint: "SHA256:test".into(), new_host: true,
        }))).unwrap();
        ui.poll();
        assert!(ui.preview.is_some() && ui.busy());
        assert!(!ui.trust_new);
        ui.probe();
        assert!(ui.receiver.is_none());
        assert_eq!(ui.password, "secret");
        ui.error("changed fingerprint".into());
        assert!(ui.password.is_empty() && ui.preview.is_none());
    }

    #[test]
    fn validation_and_partial_failure_show_dialog_and_keep_logs() {
        let mut ui = FrameUi::new(true);
        ui.ip = "invalid".into();
        ui.probe();
        let validation = ui.error_dialog.take().expect("validation dialog");
        assert!(ui.lines.last().unwrap().contains(&validation));
        assert!(ui.take_log().last().unwrap().contains(&validation));

        let (tx, rx) = mpsc::channel();
        ui.receiver = Some(rx);
        tx.send(Event::Log("Runtime setting applied".into())).unwrap();
        tx.send(Event::Log("[ERROR] Config write failed".into())).unwrap();
        tx.send(Event::Execute(Outcome {
            result: frame::SetupResult::Partial,
            error: Some("Config write failed".into()),
        })).unwrap();
        ui.poll();
        assert!(ui.error_dialog.is_some());
        assert!(ui.status_error);
        assert!(ui.lines.iter().any(|line| line == "Runtime setting applied"));
        assert!(ui.take_log().iter().any(|line| line == "[ERROR] Config write failed"));
    }
}

include!("gpui_frame_view.rs");
