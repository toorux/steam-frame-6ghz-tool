//! Presentation of current-program and retained-service logs; no device access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    #[default]
    Info,
    Warning,
    Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Program,
    Service,
}
impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Program => "程序",
            Self::Service => "服务",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Program,
    Service,
}
impl Filter {
    pub fn matches(self, source: Source) -> bool {
        matches!(
            (self, source),
            (Self::All, _) | (Self::Program, Source::Program) | (Self::Service, Source::Service)
        )
    }
}
#[derive(Clone, Debug)]
pub struct Row {
    pub time: String,
    pub source: Source,
    pub level: Level,
    pub text: String,
}

pub fn level(text: &str) -> Level {
    if text.contains("[ERROR]")
        || text.strip_prefix("[exit ").and_then(|s| s.strip_suffix(']'))
            .and_then(|s| s.parse::<i32>().ok()).is_some_and(|code| code != 0)
        || text.contains("Err(")
        || text.contains("uncertain=true")
        || [
            "权限不足",
            "拒绝访问",
            "操作结果不确定",
            "执行中断",
            "意外终止",
            "自动应用已暂停",
            "自动应用暂停",
            "超时；",
            "本次失败",
            "失败：",
            "失败 /",
            "失败:",
            "错误：",
            "错误 ",
            "Error:",
            "error:",
            "Permission denied",
            "No such file or directory",
        ]
        .iter()
        .any(|s| text.contains(s))
        || text.split("result=").skip(1).any(|s| {
            s.split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| n != 0)
        })
    {
        Level::Error
    } else if text.contains("[WARN]") || text.contains("未验证") || text.contains("尚未验证")
    {
        Level::Warning
    } else {
        Level::Info
    }
}
fn parse(text: &str, source: Source) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut time = String::new();
    let mut inherited = Level::Info;
    for line in text.lines() {
        // File headings stay in export; they aren't execution events.
        if line.starts_with("--- ") && line.ends_with(" ---") {
            continue;
        }
        let timestamp = line
            .strip_prefix('[')
            .and_then(|s| s.split_once("] "))
            .filter(|(stamp, _)| {
                stamp.len() == 24 && stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T'
            });
        let body = if let Some((stamp, body)) = timestamp {
            time = stamp.to_owned();
            inherited = level(body);
            body
        } else {
            line
        };
        let own = level(body);
        let severity = if own == Level::Info { inherited } else { own };
        rows.push(Row {
            time: time.clone(),
            source,
            level: severity,
            text: body
                .strip_prefix("[ERROR] ")
                .or_else(|| body.strip_prefix("[WARN] "))
                .unwrap_or(body)
                .to_owned(),
        });
    }
    rows
}
pub fn merge(program: &str, service: &str) -> Vec<Row> {
    let mut rows = parse(program, Source::Program);
    rows.extend(parse(service, Source::Service));
    rows.sort_by(|a, b| a.time.cmp(&b.time));
    rows
}

/// Convert a stored UTC timestamp for display only; exported logs remain UTC.
pub fn local_time(stamp: &str) -> String {
    use windows_sys::Win32::{Foundation::SYSTEMTIME, System::Time::SystemTimeToTzSpecificLocalTime};
    let parsed = || -> Option<SYSTEMTIME> {
        if stamp.len() != 24 || !stamp.ends_with('Z') { return None; }
        let part = |start, end| stamp.get(start..end)?.parse().ok();
        Some(SYSTEMTIME {
            wYear: part(0, 4)?, wMonth: part(5, 7)?, wDayOfWeek: 0,
            wDay: part(8, 10)?, wHour: part(11, 13)?, wMinute: part(14, 16)?,
            wSecond: part(17, 19)?, wMilliseconds: part(20, 23)?,
        })
    };
    let Some(utc) = parsed() else { return stamp.to_owned(); };
    let mut local = utc;
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return stamp.to_owned();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond, local.wMilliseconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merges_sources_in_time_order_and_keeps_multiline_errors_red() {
        let rows = merge(
            "[2026-09-26T08:00:02.000Z] [ERROR] 权限不足\n请以管理员身份运行\n",
            "--- auto-test.log ---\n[2026-09-26T08:00:01.000Z] result=0, bytes=336\n[2026-09-26T08:00:03.000Z] 执行结果：Err(拒绝)；uncertain=true\n",
        );
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].source, Source::Service);
        assert_eq!(rows[0].level, Level::Info);
        assert_eq!(rows[1].level, Level::Error);
        assert_eq!(rows[2].level, Level::Error);
        assert_eq!(rows[3].level, Level::Error);
        assert!(Filter::All.matches(Source::Service));
        assert!(!Filter::Program.matches(Source::Service));
        assert!(Filter::Service.matches(Source::Service));
    }
    #[test]
    fn errors_are_distinct_from_warnings_and_success() {
        for text in [
            "result=5, bytes=0",
            "错误：模拟失败",
            "操作结果不确定",
            "自动应用暂停",
        ] {
            assert_eq!(level(text), Level::Error);
        }
        for text in [
            "result=0, bytes=336",
            "执行结果：Ok(US)；uncertain=false",
            "遇到异常时不自动重试",
        ] {
            assert_eq!(level(text), Level::Info);
        }
        assert_eq!(level("驱动版本未验证"), Level::Warning);
    }

    #[test]
    fn timestamps_are_localized_only_for_display() {
        let stored = "2026-09-26T08:00:02.000Z";
        assert_eq!(local_time("invalid"), "invalid");
        assert!(!local_time(stored).ends_with('Z'));
        assert_eq!(merge(&format!("[{stored}] event"), "")[0].time, stored);
    }
}
