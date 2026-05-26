use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::log::{verbose, PhaseGuard};

pub const YTDLP_TIMEOUT: Duration = Duration::from_secs(180);
pub const WHISPER_TIMEOUT: Duration = Duration::from_secs(900);
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub fn command_output_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> io::Result<Output> {
    verbose(format!("exec {command:?} (timeout {}s)", timeout.as_secs()));
    let _phase = PhaseGuard::begin("subprocess");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout_handle = std::thread::spawn(move || read_to_vec(stdout));
    let stderr_handle = std::thread::spawn(move || read_to_vec(stderr));

    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let output = Output {
                    status,
                    stdout: join_reader(stdout_handle)?,
                    stderr: join_reader(stderr_handle)?,
                };
                verbose(format!(
                    "subprocess finished status={} elapsed={}ms stdout={}B stderr={}B",
                    output.status,
                    start.elapsed().as_millis(),
                    output.stdout.len(),
                    output.stderr.len()
                ));
                return Ok(output);
            }
            None if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                verbose(format!(
                    "subprocess timed out after {}s: {command:?}",
                    timeout.as_secs()
                ));
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("command timed out after {}s", timeout.as_secs()),
                ));
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn read_to_vec(mut pipe: impl Read + Send + 'static) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    pipe.read_to_end(&mut buf)?;
    Ok(buf)
}

fn join_reader(
    handle: std::thread::JoinHandle<io::Result<Vec<u8>>>,
) -> io::Result<Vec<u8>> {
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::Other,
            "subprocess output reader thread panicked",
        )),
    }
}

pub fn http_client(user_agent: &str) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("http client")
}
