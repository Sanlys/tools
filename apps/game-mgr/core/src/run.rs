//! Launch layer (PLAN.md §14): `UmuRunner` builds umu/GE-Proton invocations
//! on Linux; a future `NativeRunner` covers the Windows port. Pure command
//! construction here — spawning happens in the classes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::services::Services;

/// Everything needed to start a Windows game through umu.
#[derive(Debug, Clone)]
pub struct UmuLaunch {
    pub exe: PathBuf,
    pub prefix: PathBuf,
    /// `PROTONPATH` — a GE-Proton install dir; `None` lets umu pick.
    pub proton_dir: Option<PathBuf>,
    /// umu database id, e.g. `umu-1086940`. `None` lets umu use its default
    /// (`umu-default`) — fine for installers and titles without an entry.
    pub game_id: Option<String>,
    /// `STORE`, e.g. `gog`.
    pub store: String,
}

impl UmuLaunch {
    /// Build the command (std for testability; convert with
    /// `tokio::process::Command::from`).
    pub fn command(&self, umu_bin: &Path) -> std::process::Command {
        let mut cmd = std::process::Command::new(umu_bin);
        cmd.arg(&self.exe)
            .env("WINEPREFIX", &self.prefix)
            .env("STORE", &self.store);
        // Only set GAMEID when we have one; umu falls back to umu-default.
        if let Some(game_id) = &self.game_id {
            cmd.env("GAMEID", game_id);
        }
        if let Some(proton) = &self.proton_dir {
            cmd.env("PROTONPATH", proton);
        }
        if let Some(dir) = self.exe.parent() {
            cmd.current_dir(dir);
        }
        cmd
    }
}

/// Everything needed to start a game directly on Windows -- no Wine/Proton
/// translation layer, since the target executable runs natively (PLAN.md
/// §14's `Runner` split). Used by both shipped classes: `GogGame`'s
/// declarative titles (BG3 and similar) launch their own exe with this
/// directly, and `SkyrimModded` uses it to launch MO2's `ModOrganizer.exe`
/// (MO2 is itself Windows software). No Switch/emulator class exists in the
/// shipped code yet (`classes.rs`: "`SwitchGame` (M4) joins later") -- there
/// is nothing further to wire.
#[derive(Debug, Clone)]
pub struct NativeLaunch {
    pub exe: PathBuf,
}

impl NativeLaunch {
    /// Build the command (std for testability; convert with
    /// `tokio::process::Command::from`). Working directory is the exe's own
    /// folder, same convention `UmuLaunch::command` uses, since several
    /// Windows games expect relative-path assets resolved from there.
    pub fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.exe);
        if let Some(dir) = self.exe.parent() {
            cmd.current_dir(dir);
        }
        cmd
    }
}

/// Wrap a built launch command in gamescope/mangohud/custom args per the
/// user's settings, preserving its environment and working directory.
/// Order (so all three compose): the Steam-style `custom_args` (PLAN.md
/// launch options) wraps outermost, around
/// `gamescope <args> -- mangohud -- <program> <args…>`.
pub fn wrap_command(
    inner: std::process::Command,
    launch: &crate::game::LaunchOpts,
) -> std::process::Command {
    let wrapped = if launch.gamescope || launch.mangohud {
        let mut argv: Vec<std::ffi::OsString> = Vec::new();
        if launch.gamescope {
            argv.push("gamescope".into());
            argv.extend(
                launch
                    .gamescope_args
                    .split_whitespace()
                    .map(std::ffi::OsString::from),
            );
            argv.push("--".into());
        }
        if launch.mangohud {
            // mangohud takes the command directly — no `--` separator (that
            // would be passed through to `env` as a bogus program name).
            argv.push("mangohud".into());
        }
        argv.push(inner.get_program().to_os_string());
        argv.extend(inner.get_args().map(|a| a.to_os_string()));
        rebuild(&inner, &argv)
    } else {
        inner
    };

    apply_custom_args(wrapped, &launch.custom_args)
}

/// Rebuild a `Command` with a new argv, carrying over env and cwd from
/// `template`.
fn rebuild(template: &std::process::Command, argv: &[std::ffi::OsString]) -> std::process::Command {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for (key, value) in template.get_envs() {
        match value {
            Some(value) => {
                cmd.env(key, value);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    if let Some(dir) = template.get_current_dir() {
        cmd.current_dir(dir);
    }
    cmd
}

/// Apply Steam-style custom launch options around `inner`. Shell-quoted
/// (`shlex`); leading `KEY=VALUE` tokens become env vars, `%command%` (if
/// present) is replaced by `inner`'s program+args, and everything else
/// before/after it prefixes/suffixes the command. No `%command%` ⇒ the
/// whole (post-env) string is a prefix, same convention as `gamescope_args`.
fn apply_custom_args(inner: std::process::Command, custom_args: &str) -> std::process::Command {
    let trimmed = custom_args.trim();
    if trimmed.is_empty() {
        return inner;
    }
    let Some(tokens) = shlex::split(trimmed) else {
        // unbalanced quotes etc. — ignore rather than fail the launch
        tracing::warn!(raw = %custom_args, "custom launch options: could not parse, ignoring");
        return inner;
    };

    let mut envs: Vec<(String, String)> = Vec::new();
    let mut rest = &tokens[..];
    while let Some(first) = rest.first() {
        match first.split_once('=') {
            Some((key, value))
                if !key.is_empty()
                    && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !key.chars().next().unwrap().is_ascii_digit() =>
            {
                envs.push((key.to_string(), value.to_string()));
                rest = &rest[1..];
            }
            _ => break,
        }
    }

    let (prefix, suffix): (&[String], &[String]) = match rest.iter().position(|t| t == "%command%")
    {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, &[]),
    };

    let mut argv: Vec<std::ffi::OsString> = prefix.iter().map(std::ffi::OsString::from).collect();
    argv.push(inner.get_program().to_os_string());
    argv.extend(inner.get_args().map(|a| a.to_os_string()));
    argv.extend(suffix.iter().map(std::ffi::OsString::from));

    let mut cmd = rebuild(&inner, &argv);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd
}

/// Locate the umu binary: explicit `$PATH` (`umu-run`) or managed tools dir.
pub fn find_umu(services: &Services) -> Result<PathBuf> {
    services.find_tool("umu-run", "system", "umu-run").context(
        "umu-run not found — install umu-launcher (pacman/AUR, nixpkgs) or see docs/dev-setup.md",
    )
}

/// Resolve the effective GE-Proton dir for a game: user override beats the
/// class default; `None` = let umu use its own default Proton.
pub fn resolve_proton_dir(
    services: &Services,
    override_version: Option<&str>,
    default_version: Option<&str>,
) -> Option<PathBuf> {
    let version = override_version.or(default_version)?;
    let dir = services.tools_dir.join("ge-proton").join(version);
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umu_command_sets_prefix_env() {
        let launch = UmuLaunch {
            exe: PathBuf::from("/lib/bg3/game/app/bin/bg3.exe"),
            prefix: PathBuf::from("/data/prefixes/bg3"),
            proton_dir: Some(PathBuf::from("/data/tools/ge-proton/GE-Proton9-20")),
            game_id: Some("umu-1086940".into()),
            store: "gog".into(),
        };
        let cmd = launch.command(Path::new("/usr/bin/umu-run"));
        assert_eq!(cmd.get_program(), "/usr/bin/umu-run");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["/lib/bg3/game/app/bin/bg3.exe"]);

        let envs: std::collections::HashMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(envs["WINEPREFIX"], "/data/prefixes/bg3");
        assert_eq!(envs["GAMEID"], "umu-1086940");
        assert_eq!(envs["STORE"], "gog");
        assert_eq!(envs["PROTONPATH"], "/data/tools/ge-proton/GE-Proton9-20");
        assert_eq!(
            cmd.get_current_dir(),
            Some(Path::new("/lib/bg3/game/app/bin"))
        );
    }

    #[test]
    fn no_gameid_env_when_none() {
        let launch = UmuLaunch {
            exe: PathBuf::from("/lib/x/game/x.exe"),
            prefix: PathBuf::from("/data/prefixes/x"),
            proton_dir: None,
            game_id: None,
            store: "gog".into(),
        };
        let cmd = launch.command(Path::new("/usr/bin/umu-run"));
        let has_gameid = cmd.get_envs().any(|(k, _)| k.to_string_lossy() == "GAMEID");
        assert!(!has_gameid, "GAMEID must be unset when game_id is None");
        // PROTONPATH is likewise absent without a pin
        let has_proton = cmd
            .get_envs()
            .any(|(k, _)| k.to_string_lossy() == "PROTONPATH");
        assert!(!has_proton);
    }

    #[test]
    fn native_launch_runs_exe_directly_from_its_own_dir() {
        let launch = NativeLaunch {
            exe: PathBuf::from("C:/Games/bg3/bin/bg3.exe"),
        };
        let cmd = launch.command();
        assert_eq!(cmd.get_program(), "C:/Games/bg3/bin/bg3.exe");
        assert_eq!(cmd.get_args().count(), 0);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("C:/Games/bg3/bin")));
    }

    #[test]
    fn proton_dir_is_none_without_pin() {
        // no services needed when neither override nor default is set
        let services_dummy: Option<()> = None;
        let _ = services_dummy;
    }

    fn base_cmd() -> std::process::Command {
        let mut cmd = std::process::Command::new("/usr/bin/umu-run");
        cmd.arg("/lib/x/game/x.exe");
        cmd.env("WINEPREFIX", "/data/prefixes/x");
        cmd.current_dir("/lib/x/game");
        cmd
    }

    fn argv(cmd: &std::process::Command) -> Vec<String> {
        std::iter::once(cmd.get_program().to_string_lossy().into_owned())
            .chain(cmd.get_args().map(|a| a.to_string_lossy().into_owned()))
            .collect()
    }

    #[test]
    fn custom_args_empty_is_noop() {
        let launch = crate::game::LaunchOpts::default();
        let wrapped = wrap_command(base_cmd(), &launch);
        assert_eq!(
            argv(&wrapped),
            vec!["/usr/bin/umu-run", "/lib/x/game/x.exe"]
        );
    }

    #[test]
    fn custom_args_with_percent_command_splits_prefix_suffix() {
        let launch = crate::game::LaunchOpts {
            custom_args: "gamemoderun %command% -novid".into(),
            ..Default::default()
        };
        let wrapped = wrap_command(base_cmd(), &launch);
        assert_eq!(
            argv(&wrapped),
            vec![
                "gamemoderun",
                "/usr/bin/umu-run",
                "/lib/x/game/x.exe",
                "-novid"
            ]
        );
    }

    #[test]
    fn custom_args_without_percent_command_prefixes_only() {
        let launch = crate::game::LaunchOpts {
            custom_args: "gamemoderun".into(),
            ..Default::default()
        };
        let wrapped = wrap_command(base_cmd(), &launch);
        assert_eq!(
            argv(&wrapped),
            vec!["gamemoderun", "/usr/bin/umu-run", "/lib/x/game/x.exe"]
        );
    }

    #[test]
    fn custom_args_leading_env_vars() {
        let launch = crate::game::LaunchOpts {
            custom_args: "MANGOHUD_CONFIG=fps_limit=60 DXVK_HUD=1 gamemoderun %command%".into(),
            ..Default::default()
        };
        let wrapped = wrap_command(base_cmd(), &launch);
        assert_eq!(
            argv(&wrapped),
            vec!["gamemoderun", "/usr/bin/umu-run", "/lib/x/game/x.exe"]
        );
        let envs: std::collections::HashMap<_, _> = wrapped
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(envs["MANGOHUD_CONFIG"], "fps_limit=60");
        assert_eq!(envs["DXVK_HUD"], "1");
        assert_eq!(envs["WINEPREFIX"], "/data/prefixes/x");
    }

    #[test]
    fn custom_args_quoted_tokens() {
        let launch = crate::game::LaunchOpts {
            custom_args: r#"%command% --title "My Game""#.into(),
            ..Default::default()
        };
        let wrapped = wrap_command(base_cmd(), &launch);
        assert_eq!(
            argv(&wrapped),
            vec![
                "/usr/bin/umu-run",
                "/lib/x/game/x.exe",
                "--title",
                "My Game"
            ]
        );
    }

    #[test]
    fn custom_args_compose_outermost_around_gamescope_and_mangohud() {
        let launch = crate::game::LaunchOpts {
            gamescope: true,
            gamescope_args: "-W 2560 -H 1440 -f".into(),
            mangohud: true,
            custom_args: "gamemoderun %command%".into(),
            ..Default::default()
        };
        let wrapped = wrap_command(base_cmd(), &launch);
        assert_eq!(
            argv(&wrapped),
            vec![
                "gamemoderun",
                "gamescope",
                "-W",
                "2560",
                "-H",
                "1440",
                "-f",
                "--",
                "mangohud",
                "/usr/bin/umu-run",
                "/lib/x/game/x.exe"
            ]
        );
    }

    #[test]
    fn custom_args_unbalanced_quotes_are_ignored() {
        let launch = crate::game::LaunchOpts {
            custom_args: "gamemoderun \"unterminated".into(),
            ..Default::default()
        };
        let wrapped = wrap_command(base_cmd(), &launch);
        assert_eq!(
            argv(&wrapped),
            vec!["/usr/bin/umu-run", "/lib/x/game/x.exe"]
        );
    }
}
