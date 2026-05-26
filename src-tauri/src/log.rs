use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn init_logging(args: impl IntoIterator<Item = impl AsRef<str>>) {
    if verbose_env_enabled() {
        VERBOSE.store(true, Ordering::Relaxed);
    }
    for arg in args {
        let arg = arg.as_ref();
        if arg == "--verbose" || arg == "-v" {
            VERBOSE.store(true, Ordering::Relaxed);
        }
    }
    if verbose_enabled() {
        eprintln!("[kpopmvlyrics] verbose logging enabled");
    }
}

/// Remove flags consumed by this app so GTK/GApplication does not reject them.
pub fn filter_app_args(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .enumerate()
        .filter(|(index, arg)| {
            *index == 0 || (arg != "--verbose" && arg != "-v")
        })
        .map(|(_, arg)| arg)
        .collect()
}

fn verbose_env_enabled() -> bool {
    match std::env::var("KPOPMVLYRICS_VERBOSE") {
        Ok(value) => {
            let lower = value.to_ascii_lowercase();
            !matches!(lower.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

pub fn verbose_enabled() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn verbose(message: impl AsRef<str>) {
    if !verbose_enabled() {
        return;
    }
    let _ = writeln!(
        io::stderr(),
        "[kpopmvlyrics {:.3}] {}",
        elapsed_since_start(),
        message.as_ref()
    );
}

pub fn progress(stage: &str, fraction: f64) {
    verbose(format!("progress {fraction:.2} {stage}"));
}

pub struct PhaseGuard {
    name: &'static str,
    started: Instant,
}

impl PhaseGuard {
    pub fn begin(name: &'static str) -> Self {
        verbose(format!(">>> {name}"));
        Self {
            name,
            started: Instant::now(),
        }
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        verbose(format!(
            "<<< {} ({} ms)",
            self.name,
            self.started.elapsed().as_millis()
        ));
    }
}

fn elapsed_since_start() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_secs_f64()
}
