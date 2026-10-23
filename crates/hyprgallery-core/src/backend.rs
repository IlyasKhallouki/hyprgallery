//! Putting a wallpaper on the screen.
//!
//! hyprgallery does not draw wallpapers itself. It drives whichever wallpaper
//! program is already in use, so it fits into a setup rather than replacing
//! part of it. Building the commands ([`plan`]) is kept apart from running them
//! ([`execute`]) so every backend's exact command line is unit tested.

use crate::config::{BackendChoice, Config};
use crate::library::Kind;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// HyDE's own wallpaper script, which also recolours the theme from the
    /// image and restores the wallpaper at login by itself.
    Hyde,
    /// awww, or its predecessor swww, which share a command line.
    Awww {
        bin: &'static str,
    },
    Hyprpaper,
    Swaybg,
    /// The user's own command. `{path}` is replaced with the file.
    Command(String),
}

impl Backend {
    pub fn name(&self) -> &str {
        match self {
            Backend::Hyde => "hyde",
            Backend::Awww { bin } => bin,
            Backend::Hyprpaper => "hyprpaper",
            Backend::Swaybg => "swaybg",
            Backend::Command(_) => "command",
        }
    }

    /// Does this backend bring the wallpaper back at login on its own?
    pub fn restores_itself(&self) -> bool {
        matches!(self, Backend::Hyde)
    }

    /// Does applying take long enough that the gallery should say so?
    pub fn is_slow(&self) -> bool {
        matches!(self, Backend::Hyde)
    }
}

/// What the machine has. A trait so detection can be tested without it.
pub trait Probe {
    fn has(&self, bin: &str) -> bool;
    fn running(&self, process: &str) -> bool;
    fn hyde(&self) -> bool;
}

pub struct System;

impl Probe for System {
    fn has(&self, bin: &str) -> bool {
        crate::paths::which(bin).is_some()
    }

    fn running(&self, process: &str) -> bool {
        pids_named(process).next().is_some()
    }

    fn hyde(&self) -> bool {
        self.has("hyde-shell") && crate::paths::config_home().join("hyde/themes").is_dir()
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BackendError {
    #[error(
        "no wallpaper program found. Install awww (or swww), hyprpaper or swaybg, or set `backend` in config.toml"
    )]
    NoneFound,
    #[error("backend `{0}` is not installed")]
    Missing(String),
    #[error("`{0}` cannot show {1}")]
    Unsupported(String, &'static str),
    #[error("{0}")]
    Failed(String),
}

pub fn detect(config: &Config, probe: &dyn Probe) -> Result<Backend, BackendError> {
    let need = |bin: &'static str| {
        if probe.has(bin) { Ok(()) } else { Err(BackendError::Missing(bin.into())) }
    };
    match config.backend {
        BackendChoice::Hyde => need("hyde-shell").map(|_| Backend::Hyde),
        BackendChoice::Awww => need("awww").map(|_| Backend::Awww { bin: "awww" }),
        BackendChoice::Swww => need("swww").map(|_| Backend::Awww { bin: "swww" }),
        BackendChoice::Hyprpaper => need("hyprpaper").map(|_| Backend::Hyprpaper),
        BackendChoice::Swaybg => need("swaybg").map(|_| Backend::Swaybg),
        BackendChoice::Command => Ok(Backend::Command(config.command.clone().unwrap_or_default())),
        BackendChoice::Auto => {
            if probe.hyde() {
                Ok(Backend::Hyde)
            // A running hyprpaper is a stronger signal than an installed awww,
            // which may just be a leftover.
            } else if probe.running("hyprpaper") {
                Ok(Backend::Hyprpaper)
            } else if probe.has("awww") {
                Ok(Backend::Awww { bin: "awww" })
            } else if probe.has("swww") {
                Ok(Backend::Awww { bin: "swww" })
            } else if probe.has("hyprpaper") {
                Ok(Backend::Hyprpaper)
            } else if probe.has("swaybg") {
                Ok(Backend::Swaybg)
            } else {
                Err(BackendError::NoneFound)
            }
        }
    }
}

/// How the change should look. Only awww and swww animate.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub kind: String,
    pub duration: f32,
    pub fps: u32,
    /// Where on the output the transition starts, in output pixels with the
    /// origin at the top left.
    pub at: Option<(i32, i32)>,
}

impl Transition {
    pub fn from_config(config: &Config, at: Option<(i32, i32)>) -> Self {
        Self {
            kind: config.transition.kind.clone(),
            duration: config.transition.duration,
            fps: config.transition.fps,
            at,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Run to completion; failure fails the apply.
    Run(Vec<OsString>),
    /// Run to completion; failure is ignored.
    Try(Vec<OsString>),
    /// Start a long-lived wallpaper process, then stop any older copy of it.
    /// In that order, so there is never a frame with no wallpaper.
    Replace { name: &'static str, argv: Vec<OsString> },
    /// Stop every process with this name.
    Kill(&'static str),
    /// Start the awww/swww daemon unless it is already answering.
    EnsureAwww(&'static str),
}

fn argv<I, S>(parts: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    parts.into_iter().map(Into::into).collect()
}

/// The steps that put `path` on screen with `backend`.
pub fn plan(
    backend: &Backend,
    path: &Path,
    kind: Kind,
    transition: Option<&Transition>,
) -> Result<Vec<Step>, BackendError> {
    let file: OsString = path.as_os_str().to_owned();
    // A video plays in mpvpaper, on top of whatever still wallpaper program is
    // running. Anything that is not a video has to stop it, or the old video
    // keeps covering the new image.
    let stop_video = Step::Kill("mpvpaper");
    let video = || -> Vec<Step> {
        vec![Step::Replace {
            name: "mpvpaper",
            argv: argv([
                OsString::from("mpvpaper"),
                "-o".into(),
                "no-audio loop panscan=1.0".into(),
                "ALL".into(),
                file.clone(),
            ]),
        }]
    };

    Ok(match (backend, kind) {
        (Backend::Hyde, Kind::Video) => vec![Step::Run(argv([
            OsString::from("hyde-shell"),
            "wallpaper".into(),
            "--global".into(),
            "--backend".into(),
            "mpvpaper".into(),
            "--set".into(),
            file,
        ]))],
        (Backend::Hyde, _) => vec![
            stop_video,
            Step::Run(argv([
                OsString::from("hyde-shell"),
                "wallpaper".into(),
                "--global".into(),
                "--set".into(),
                file,
            ])),
        ],

        (Backend::Awww { .. } | Backend::Hyprpaper | Backend::Swaybg, Kind::Video) => video(),

        (Backend::Awww { bin }, _) => {
            let mut cmd = argv([OsString::from(*bin), "img".into(), file]);
            match transition {
                Some(t) => {
                    cmd.extend(argv([
                        "--transition-type".to_owned(),
                        t.kind.clone(),
                        "--transition-duration".into(),
                        format!("{}", t.duration),
                        "--transition-fps".into(),
                        t.fps.to_string(),
                    ]));
                    if let Some((x, y)) = t.at {
                        // awww measures from the bottom unless told otherwise.
                        cmd.extend(argv([
                            "--transition-pos".to_owned(),
                            format!("{x},{y}"),
                            "--invert-y".into(),
                        ]));
                    }
                }
                None => cmd.extend(argv(["--transition-type", "none"])),
            }
            vec![stop_video, Step::EnsureAwww(bin), Step::Run(cmd)]
        }

        (Backend::Hyprpaper, _) => {
            let target = format!(",{}", path.display());
            vec![
                stop_video,
                // Older hyprpaper needs a preload first; newer ones load on
                // demand and reject the request, which is harmless.
                Step::Try(argv([
                    OsString::from("hyprctl"),
                    "hyprpaper".into(),
                    "preload".into(),
                    file,
                ])),
                Step::Run(argv(["hyprctl", "hyprpaper", "wallpaper", &target])),
                Step::Try(argv(["hyprctl", "hyprpaper", "unload", "unused"])),
            ]
        }

        (Backend::Swaybg, _) => vec![
            stop_video,
            Step::Replace {
                name: "swaybg",
                argv: argv([
                    OsString::from("swaybg"),
                    "-m".into(),
                    "fill".into(),
                    "-i".into(),
                    file,
                ]),
            },
        ],

        (Backend::Command(template), _) => {
            // The path goes in as a positional parameter, never spliced into
            // the script, so no file name can inject shell.
            let script = template.replace("{path}", "\"$1\"");
            vec![Step::Run(argv([
                OsString::from("sh"),
                "-c".into(),
                script.into(),
                "hyprgallery".into(),
                file,
            ]))]
        }
    })
}

/// Run a plan. Blocks, so call it off the UI thread.
pub fn execute(steps: &[Step]) -> Result<(), BackendError> {
    for step in steps {
        match step {
            Step::Run(cmd) => run(cmd)?,
            Step::Try(cmd) => {
                if let Err(e) = run(cmd) {
                    tracing::debug!("ignored: {e}");
                }
            }
            Step::Kill(name) => {
                for pid in pids_named(name) {
                    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                }
            }
            Step::Replace { name, argv } => {
                let old: Vec<u32> = pids_named(name).collect();
                let child = spawn_detached(argv)?;
                // Give the new one a moment to draw before the old one goes, or
                // the desktop flashes the compositor background in between.
                std::thread::sleep(Duration::from_millis(250));
                for pid in old.into_iter().filter(|&p| p != child) {
                    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                }
            }
            Step::EnsureAwww(bin) => ensure_awww(bin)?,
        }
    }
    Ok(())
}

fn run(cmd: &[OsString]) -> Result<(), BackendError> {
    let (program, args) =
        cmd.split_first().ok_or_else(|| BackendError::Failed("empty command".into()))?;
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| BackendError::Failed(format!("{}: {e}", program.to_string_lossy())))?;
    // hyprctl reports most failures on stdout with a zero exit code.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let refused = program == "hyprctl"
        && !stdout.trim().is_empty()
        && !stdout.trim().eq_ignore_ascii_case("ok");
    if out.status.success() && !refused {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let why = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("exited with an error")
        .to_owned();
    Err(BackendError::Failed(format!("{}: {why}", program.to_string_lossy())))
}

fn ensure_awww(bin: &str) -> Result<(), BackendError> {
    let answering = || {
        Command::new(bin)
            .arg("query")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if answering() {
        return Ok(());
    }
    let daemon = format!("{bin}-daemon");
    spawn_detached(&argv([daemon.as_str()]))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if answering() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(BackendError::Failed(format!("{daemon} did not start")))
}

/// Start a process that outlives us and belongs to nobody's terminal.
pub fn spawn_detached(cmd: &[OsString]) -> Result<u32, BackendError> {
    use std::os::unix::process::CommandExt;
    let (program, args) =
        cmd.split_first().ok_or_else(|| BackendError::Failed("empty command".into()))?;
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // New session: a wallpaper must not die with the terminal or daemon that
    // happened to start it.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|e| BackendError::Failed(format!("{}: {e}", program.to_string_lossy())))?;
    let pid = child.id();
    // Reap it whenever it exits so it never lingers as a zombie.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(pid)
}

/// Processes owned by this user whose command name is exactly `name`.
pub fn pids_named(name: &str) -> impl Iterator<Item = u32> + '_ {
    let uid = unsafe { libc::getuid() };
    std::fs::read_dir("/proc").into_iter().flatten().flatten().filter_map(move |entry| {
        let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
        let comm = std::fs::read_to_string(entry.path().join("comm")).ok()?;
        if comm.trim_end() != name {
            return None;
        }
        use std::os::unix::fs::MetadataExt;
        (entry.metadata().ok()?.uid() == uid).then_some(pid)
    })
}

/// What awww is showing right now, if it is running. Used to skip a restore
/// that would only redraw the same image.
pub fn awww_current(bin: &str) -> Vec<PathBuf> {
    let Ok(out) = Command::new(bin).arg("query").stderr(Stdio::null()).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once("image: ").map(|(_, p)| PathBuf::from(p.trim())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct Fake {
        bins: HashSet<&'static str>,
        running: HashSet<&'static str>,
        hyde: bool,
    }

    impl Probe for Fake {
        fn has(&self, bin: &str) -> bool {
            self.bins.contains(bin)
        }
        fn running(&self, p: &str) -> bool {
            self.running.contains(p)
        }
        fn hyde(&self) -> bool {
            self.hyde
        }
    }

    fn fake(bins: &[&'static str], running: &[&'static str], hyde: bool) -> Fake {
        Fake {
            bins: bins.iter().copied().collect(),
            running: running.iter().copied().collect(),
            hyde,
        }
    }

    fn strings(step: &Step) -> Vec<String> {
        let cmd = match step {
            Step::Run(c) | Step::Try(c) => c,
            Step::Replace { argv, .. } => argv,
            other => panic!("not a command: {other:?}"),
        };
        cmd.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn auto_prefers_hyde_then_a_running_hyprpaper() {
        let config = Config::default();
        assert_eq!(detect(&config, &fake(&["awww"], &[], true)), Ok(Backend::Hyde));
        assert_eq!(
            detect(&config, &fake(&["awww", "hyprpaper"], &["hyprpaper"], false)),
            Ok(Backend::Hyprpaper)
        );
        assert_eq!(
            detect(&config, &fake(&["awww", "hyprpaper"], &[], false)),
            Ok(Backend::Awww { bin: "awww" })
        );
        assert_eq!(
            detect(&config, &fake(&["swww"], &[], false)),
            Ok(Backend::Awww { bin: "swww" })
        );
        assert_eq!(detect(&config, &fake(&["swaybg"], &[], false)), Ok(Backend::Swaybg));
        assert_eq!(detect(&config, &fake(&[], &[], false)), Err(BackendError::NoneFound));
    }

    #[test]
    fn explicit_choice_must_be_installed() {
        let config = Config { backend: BackendChoice::Swaybg, ..Config::default() };
        assert_eq!(
            detect(&config, &fake(&["awww"], &[], false)),
            Err(BackendError::Missing("swaybg".into()))
        );
    }

    #[test]
    fn hyde_goes_through_hyde_shell() {
        let steps = plan(&Backend::Hyde, Path::new("/w/a b.jpg"), Kind::Still, None).unwrap();
        assert_eq!(steps[0], Step::Kill("mpvpaper"));
        assert_eq!(
            strings(&steps[1]),
            ["hyde-shell", "wallpaper", "--global", "--set", "/w/a b.jpg"]
        );

        let steps = plan(&Backend::Hyde, Path::new("/w/v.mp4"), Kind::Video, None).unwrap();
        assert_eq!(
            strings(&steps[0]),
            ["hyde-shell", "wallpaper", "--global", "--backend", "mpvpaper", "--set", "/w/v.mp4"]
        );
    }

    #[test]
    fn awww_grows_from_the_click() {
        let t = Transition { kind: "grow".into(), duration: 0.9, fps: 60, at: Some((300, 200)) };
        let steps =
            plan(&Backend::Awww { bin: "awww" }, Path::new("/w/a.jpg"), Kind::Still, Some(&t))
                .unwrap();
        assert_eq!(steps[1], Step::EnsureAwww("awww"));
        assert_eq!(
            strings(&steps[2]),
            [
                "awww",
                "img",
                "/w/a.jpg",
                "--transition-type",
                "grow",
                "--transition-duration",
                "0.9",
                "--transition-fps",
                "60",
                "--transition-pos",
                "300,200",
                "--invert-y"
            ]
        );
        let steps =
            plan(&Backend::Awww { bin: "swww" }, Path::new("/w/a.jpg"), Kind::Still, None).unwrap();
        assert_eq!(strings(&steps[2]), ["swww", "img", "/w/a.jpg", "--transition-type", "none"]);
    }

    #[test]
    fn video_uses_mpvpaper_and_stills_stop_it() {
        let steps = plan(&Backend::Swaybg, Path::new("/w/v.webm"), Kind::Video, None).unwrap();
        assert!(matches!(&steps[0], Step::Replace { name: "mpvpaper", .. }));
        let steps = plan(&Backend::Swaybg, Path::new("/w/a.png"), Kind::Still, None).unwrap();
        assert_eq!(steps[0], Step::Kill("mpvpaper"));
        assert_eq!(strings(&steps[1]), ["swaybg", "-m", "fill", "-i", "/w/a.png"]);
    }

    #[test]
    fn hyprpaper_sets_every_monitor() {
        let steps = plan(&Backend::Hyprpaper, Path::new("/w/a.png"), Kind::Still, None).unwrap();
        assert_eq!(strings(&steps[2]), ["hyprctl", "hyprpaper", "wallpaper", ",/w/a.png"]);
    }

    #[test]
    fn command_quotes_the_path_as_an_argument() {
        let steps = plan(
            &Backend::Command("setwall {path} --now".into()),
            Path::new("/w/$(rm -rf).jpg"),
            Kind::Still,
            None,
        )
        .unwrap();
        assert_eq!(
            strings(&steps[0]),
            ["sh", "-c", "setwall \"$1\" --now", "hyprgallery", "/w/$(rm -rf).jpg"]
        );
    }

    #[test]
    fn command_backend_really_receives_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.txt");
        let template = format!("printf %s {{path}} > '{}'", out.display());
        let steps =
            plan(&Backend::Command(template), Path::new("/w/it's here.jpg"), Kind::Still, None)
                .unwrap();
        execute(&steps).unwrap();
        assert_eq!(std::fs::read_to_string(out).unwrap(), "/w/it's here.jpg");
    }

    #[test]
    fn failure_reports_the_last_line_of_stderr() {
        let steps = plan(
            &Backend::Command("echo first >&2; echo 'no such output' >&2; exit 3".into()),
            Path::new("/x"),
            Kind::Still,
            None,
        )
        .unwrap();
        let err = execute(&steps).unwrap_err().to_string();
        assert!(err.ends_with("no such output"), "{err}");
    }
}
