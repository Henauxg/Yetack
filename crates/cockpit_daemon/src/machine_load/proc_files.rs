//! The `/proc` files the sampling reads, and their parsing. A line that does not parse gives
//! `None`: the caller skips the process, or reports the machine's load as unknown.

use cockpit_protocol::{LoadAverage, ProcessId};

pub(super) const PROC_ROOT: &str = "/proc";
pub(super) const STAT_FILE: &str = "stat";
pub(super) const CMDLINE_FILE: &str = "cmdline";
pub(super) const TASK_FOLDER: &str = "task";
pub(super) const CHILDREN_FILE: &str = "children";
pub(super) const SERVER_STAT_PATH: &str = "/proc/stat";
pub(super) const MEMINFO_PATH: &str = "/proc/meminfo";
pub(super) const LOADAVG_PATH: &str = "/proc/loadavg";
pub(super) const UPTIME_PATH: &str = "/proc/uptime";

/// A command line past this many characters ends in an ellipsis.
const MAX_COMMAND_CHARS: usize = 120;
const ELLIPSIS: char = '…';

/// What the sampling reads of `/proc/<pid>/stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessStat {
    pub(super) key: ProcessKey,
    /// The kernel's name for it, up to 15 bytes: shown when the command line is empty.
    pub(super) name: String,
    pub(super) parent: ProcessId,
    /// CPU time of the process itself, user and system, in clock ticks.
    pub(super) own_ticks: u64,
    /// `own_ticks`, plus the CPU time of the children it waited for, which the kernel adds when a
    /// child is reaped.
    pub(super) total_ticks: u64,
    pub(super) threads: u32,
    pub(super) resident_pages: u64,
    /// It ended, and its parent has not reaped it yet: it runs nothing, and a signal does nothing.
    pub(super) zombie: bool,
}

/// A process, told apart from a later one that reuses its pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ProcessKey {
    pub(super) pid: ProcessId,
    /// Clock ticks after boot.
    pub(super) started: u64,
}

/// The fields of a `stat` line, counted from the state, which follows the name.
const STATE_FIELD: usize = 3;
const ZOMBIE_STATE: &str = "Z";
const PARENT_FIELD: usize = 4;
const USER_TICKS_FIELD: usize = 14;
const SYSTEM_TICKS_FIELD: usize = 15;
const CHILDREN_USER_TICKS_FIELD: usize = 16;
const CHILDREN_SYSTEM_TICKS_FIELD: usize = 17;
const THREADS_FIELD: usize = 20;
const START_FIELD: usize = 22;
const RESIDENT_FIELD: usize = 24;

impl ProcessStat {
    /// Parses `pid (name) state ppid ...`. The name may hold spaces and parentheses, so the
    /// fields count from its last `)`.
    pub(super) fn parse(line: &str) -> Option<Self> {
        let open = line.find('(')?;
        let close = line.rfind(')')?;
        let pid: u32 = line.get(..open)?.trim().parse().ok()?;
        let name = line.get(open + 1..close)?.to_owned();
        let fields: Vec<&str> = line.get(close + 1..)?.split_whitespace().collect();
        let field = |number: usize| fields.get(number.checked_sub(STATE_FIELD)?).copied();
        let unsigned = |number: usize| field(number)?.parse::<u64>().ok();
        // The kernel writes the children's times as signed numbers.
        let children = |number: usize| {
            let ticks: i64 = field(number)?.parse().ok()?;
            Some(u64::try_from(ticks).unwrap_or(0))
        };
        let own_ticks = unsigned(USER_TICKS_FIELD)?.saturating_add(unsigned(SYSTEM_TICKS_FIELD)?);
        let children_ticks = children(CHILDREN_USER_TICKS_FIELD)?
            .saturating_add(children(CHILDREN_SYSTEM_TICKS_FIELD)?);
        Some(Self {
            key: ProcessKey {
                pid: ProcessId::new(pid),
                started: unsigned(START_FIELD)?,
            },
            name,
            parent: ProcessId::new(field(PARENT_FIELD)?.parse().ok()?),
            own_ticks,
            total_ticks: own_ticks.saturating_add(children_ticks),
            threads: field(THREADS_FIELD)?.parse().ok()?,
            resident_pages: unsigned(RESIDENT_FIELD)?,
            zombie: field(STATE_FIELD)? == ZOMBIE_STATE,
        })
    }
}

/// The CPU time of every core together, from the `cpu` line of `/proc/stat`, in clock ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CpuTimes {
    pub(super) busy: u64,
    /// `busy` plus the idle and I/O wait times.
    pub(super) total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ServerCpu {
    pub(super) times: CpuTimes,
    /// The `cpuN` lines: one per online core.
    pub(super) cores: u16,
}

/// The first line is `cpu  user nice system idle iowait irq softirq steal guest guest_nice`. The
/// guest times are already in `user` and `nice`, so they are left out.
pub(super) fn parse_server_cpu(text: &str) -> Option<ServerCpu> {
    const CPU_PREFIX: &str = "cpu";
    const IDLE_COLUMNS: [usize; 2] = [3, 4];
    const COUNTED_COLUMNS: usize = 8;

    let mut lines = text.lines();
    let first = lines.next()?.strip_prefix(CPU_PREFIX)?;
    if !first.starts_with(char::is_whitespace) {
        return None;
    }
    let mut busy: u64 = 0;
    let mut idle: u64 = 0;
    let mut columns = 0;
    for (column, value) in first.split_whitespace().take(COUNTED_COLUMNS).enumerate() {
        let ticks: u64 = value.parse().ok()?;
        if IDLE_COLUMNS.contains(&column) {
            idle = idle.saturating_add(ticks);
        } else {
            busy = busy.saturating_add(ticks);
        }
        columns += 1;
    }
    // `user nice system idle` at least.
    if columns < 4 {
        return None;
    }
    let cores = lines
        .filter_map(|line| line.strip_prefix(CPU_PREFIX))
        .filter(|rest| rest.starts_with(|first: char| first.is_ascii_digit()))
        .count();
    Some(ServerCpu {
        times: CpuTimes {
            busy,
            total: busy.saturating_add(idle),
        },
        cores: u16::try_from(cores).unwrap_or(u16::MAX),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MemoryInfo {
    pub(super) total_bytes: u64,
    /// What the kernel could give to a new program without swapping.
    pub(super) available_bytes: u64,
}

/// Reads `MemTotal` and `MemAvailable`, which `/proc/meminfo` gives in kibibytes.
pub(super) fn parse_meminfo(text: &str) -> Option<MemoryInfo> {
    const TOTAL_KEY: &str = "MemTotal:";
    const AVAILABLE_KEY: &str = "MemAvailable:";
    const KIBIBYTE: u64 = 1024;

    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let slot = match words.next() {
            Some(TOTAL_KEY) => &mut total,
            Some(AVAILABLE_KEY) => &mut available,
            _ => continue,
        };
        let kibibytes: u64 = words.next()?.parse().ok()?;
        *slot = Some(kibibytes.saturating_mul(KIBIBYTE));
    }
    Some(MemoryInfo {
        total_bytes: total?,
        available_bytes: available?,
    })
}

/// `/proc/loadavg` is `1.25 0.75 0.50 3/812 4242`.
pub(super) fn parse_loadavg(text: &str) -> Option<LoadAverage> {
    let mut words = text.split_whitespace();
    let mut next = || words.next()?.parse::<f64>().ok();
    Some(LoadAverage {
        one_minute: next()?,
        five_minutes: next()?,
        fifteen_minutes: next()?,
    })
}

/// The seconds since boot, the first number of `/proc/uptime` (`350.25`), in clock ticks.
pub(super) fn parse_uptime_ticks(text: &str, ticks_per_second: u64) -> Option<u64> {
    let uptime = text.split_whitespace().next()?;
    let (seconds, fraction) = uptime.split_once('.').unwrap_or((uptime, ""));
    let seconds: u64 = seconds.parse().ok()?;
    let mut fraction_ticks: u64 = 0;
    let mut scale: u64 = 1;
    for digit in fraction.chars().take(6) {
        fraction_ticks = fraction_ticks * 10 + u64::from(digit.to_digit(10)?);
        scale *= 10;
    }
    let ticks = seconds
        .checked_mul(ticks_per_second)?
        .checked_add(fraction_ticks.checked_mul(ticks_per_second)? / scale)?;
    Some(ticks)
}

/// The pids of `/proc/<pid>/task/<tid>/children`: `812 4242 `.
pub(super) fn parse_children(text: &str, children: &mut Vec<ProcessId>) {
    let pids = text
        .split_whitespace()
        .filter_map(|word| word.parse::<u32>().ok())
        .map(ProcessId::new);
    children.extend(pids);
}

/// The command line of `/proc/<pid>/cmdline`, whose arguments end in NUL bytes: the program's
/// file name, then its arguments, on one line, cut at [`MAX_COMMAND_CHARS`]. `name` stands in
/// for an empty command line, as a kernel thread's or a zombie's.
pub(super) fn short_command(cmdline: &[u8], name: &str) -> String {
    const SEPARATOR: u8 = 0;

    let mut words = cmdline
        .split(|&byte| byte == SEPARATOR)
        .filter(|word| !word.is_empty());
    let Some(program) = words.next() else {
        return format!("[{name}]");
    };
    let program = program
        .rsplit(|&byte| byte == b'/')
        .next()
        .unwrap_or(program);
    let mut line = String::from_utf8_lossy(program).into_owned();
    for word in words {
        line.push(' ');
        line.push_str(&String::from_utf8_lossy(word));
        if line.chars().count() > MAX_COMMAND_CHARS {
            break;
        }
    }
    let mut short: String = line
        .chars()
        .take(MAX_COMMAND_CHARS)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if line.chars().count() > MAX_COMMAND_CHARS {
        short.push(ELLIPSIS);
    }
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell renamed to hold spaces and parentheses, with 3 threads, started 9000 ticks after
    /// boot, using 1500 pages.
    const STRANGE_NAME: &str = "4242 (sh (x) y) S 812 4242 812 34816 4242 4194560 1234 5678 0 0 \
        300 120 45 15 20 0 3 0 9000 26017792 1500 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 \
        17 5 0 0 0 0 0";

    #[test]
    fn a_stat_line_parses_whatever_the_name_holds() {
        let stat = ProcessStat::parse(STRANGE_NAME).unwrap();

        assert_eq!(
            stat,
            ProcessStat {
                key: ProcessKey {
                    pid: ProcessId::new(4242),
                    started: 9000,
                },
                name: "sh (x) y".to_owned(),
                parent: ProcessId::new(812),
                own_ticks: 420,
                total_ticks: 480,
                threads: 3,
                resident_pages: 1500,
                zombie: false,
            }
        );
    }

    #[test]
    fn a_malformed_stat_line_gives_none() {
        let cut = &STRANGE_NAME[..60];
        let lines = [
            "",
            "4242",
            "4242 (sh",
            "x (sh) S 812",
            cut,
            "4242 (sh) S 812 4242 812 0 0 0 0 0 0 0 many 120 0 0 20 0 1 0 9000 0 1500",
        ];

        for line in lines {
            assert_eq!(ProcessStat::parse(line), None, "{line}");
        }
    }

    #[test]
    fn negative_children_times_count_as_zero() {
        let line = "7 (make) R 1 7 7 0 7 0 0 0 0 0 10 5 -4 -1 20 0 1 0 50 0 200";

        let stat = ProcessStat::parse(line).unwrap();

        assert_eq!((stat.own_ticks, stat.total_ticks), (15, 15));
        assert!(!stat.zombie);
        let ended = ProcessStat::parse(&line.replace(" R ", " Z ")).unwrap();
        assert!(ended.zombie);
    }

    #[test]
    fn the_server_cpu_leaves_out_idle_and_guest_time() {
        let text = "cpu  100 20 30 800 50 5 5 10 40 0\n\
            cpu0 50 10 15 400 25 3 2 5 20 0\n\
            cpu1 50 10 15 400 25 2 3 5 20 0\n\
            intr 12345 0 0\n\
            ctxt 6789\n";

        let cpu = parse_server_cpu(text).unwrap();

        assert_eq!(
            cpu,
            ServerCpu {
                times: CpuTimes {
                    busy: 170,
                    total: 1020,
                },
                cores: 2,
            }
        );
    }

    #[test]
    fn a_server_cpu_line_too_short_or_missing_gives_none() {
        for text in [
            "",
            "cpu  100 20 30\n",
            "intr 1 2 3\n",
            "cpu0 1 2 3 4\n",
            "cpux 1 2 3 4\n",
        ] {
            assert_eq!(parse_server_cpu(text), None, "{text:?}");
        }
    }

    #[test]
    fn meminfo_gives_the_total_and_the_available_memory_in_bytes() {
        let text = "MemTotal:       16384000 kB\n\
            MemFree:         1024000 kB\n\
            MemAvailable:    8192000 kB\n\
            Buffers:          512000 kB\n";

        let memory = parse_meminfo(text).unwrap();

        assert_eq!(
            memory,
            MemoryInfo {
                total_bytes: 16_384_000 * 1024,
                available_bytes: 8_192_000 * 1024,
            }
        );
        assert_eq!(parse_meminfo("MemTotal: 16384000 kB\n"), None);
        assert_eq!(
            parse_meminfo("MemTotal: lots kB\nMemAvailable: 1 kB\n"),
            None
        );
    }

    #[test]
    fn loadavg_and_uptime_parse() {
        let average = parse_loadavg("1.25 0.75 0.50 3/812 4242\n").unwrap();

        assert_eq!(
            average,
            LoadAverage {
                one_minute: 1.25,
                five_minutes: 0.75,
                fifteen_minutes: 0.5,
            }
        );
        assert_eq!(parse_loadavg("1.25 0.75\n"), None);
        assert_eq!(parse_uptime_ticks("350.25 1400.50\n", 100), Some(35_025));
        assert_eq!(parse_uptime_ticks("12 0\n", 100), Some(1200));
        assert_eq!(parse_uptime_ticks("-1 0\n", 100), None);
        assert_eq!(parse_uptime_ticks("1.x 0\n", 100), None);
        assert_eq!(parse_uptime_ticks("", 100), None);
    }

    #[test]
    fn children_files_list_pids_and_skip_the_rest() {
        let mut children = vec![ProcessId::new(1)];

        parse_children("812 4242 \n", &mut children);
        parse_children("", &mut children);
        parse_children("x 9", &mut children);

        assert_eq!(
            children,
            [1, 812, 4242, 9].map(ProcessId::new),
            "the earlier children stay"
        );
    }

    #[test]
    fn a_command_line_shows_its_program_name_and_its_arguments() {
        let cargo = b"/home/dev/.cargo/bin/cargo\0build\0--release\0";
        let long = format!("/usr/bin/rustc\0{}\0", "--cfg feature=\"x\" ".repeat(10));
        let newline = b"sh\0-c\0echo one\necho two\0";

        assert_eq!(short_command(cargo, "cargo"), "cargo build --release");
        assert_eq!(short_command(b"", "kworker/3:1"), "[kworker/3:1]");
        assert_eq!(short_command(newline, "sh"), "sh -c echo one echo two");
        let cut = short_command(long.as_bytes(), "rustc");
        assert!(cut.starts_with("rustc --cfg"), "{cut}");
        assert_eq!(cut.chars().count(), MAX_COMMAND_CHARS + 1);
        assert!(cut.ends_with(ELLIPSIS), "{cut}");
    }
}
