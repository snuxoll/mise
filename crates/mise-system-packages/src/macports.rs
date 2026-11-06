use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use eyre::{Result, bail};

use super::{InstallOpts, PackageRequest, PackageState, PackageStatus, SystemPackageManager};
use mise_ui::sudo;

/// Where the MacPorts installer puts `port`. The installer adds `/opt/local/bin`
/// to PATH only in shell profiles, so a non-login shell, or a mise process
/// started before a bootstrap hook installed MacPorts, does not find it there.
const DEFAULT_PORT: &str = "/opt/local/bin/port";

/// Words `port` reads as pseudo-ports or operators instead of port names, such
/// as `outdated`, which names every outdated port.
const RESERVED_WORDS: &[&str] = &[
    "actinact",
    "active",
    "all",
    "and",
    "current",
    "inactive",
    "installed",
    "leaves",
    "not",
    "obsolete",
    "or",
    "outdated",
    "requested",
    "rleaves",
    "uninstalled",
    "unrequested",
];

/// MacPorts ports via `port`.
pub(crate) struct MacportsManager;

/// The `port` executable: the first on PATH, else the default prefix's. Looked
/// up on each call, because a `pre-packages` hook may have just installed it.
fn port_bin() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    mise_util::file::which("port").or_else(|| {
        let default = Path::new(DEFAULT_PORT);
        (default.is_file() && mise_util::file::is_executable(default))
            .then(|| default.to_path_buf())
    })
}

fn require_port_bin() -> Result<PathBuf> {
    match port_bin() {
        Some(port) => Ok(port),
        None => bail!("port not found on PATH or at {DEFAULT_PORT}"),
    }
}

/// Reject names `port` would parse as something other than one port: options,
/// variants, version selectors, globs, field selectors such as `name:`, or a
/// pseudo-port such as `outdated`. Port names are letters, digits, and `._+-`.
fn validate_names(pkgs: &[PackageRequest]) -> Result<()> {
    for pkg in pkgs {
        let name = &pkg.name;
        let valid = name.starts_with(|c: char| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
        if !valid {
            bail!(
                "'macports:{name}' is not a valid port name; use letters, digits, '.', '_', '+', and '-', starting with a letter or digit"
            );
        }
        if RESERVED_WORDS.contains(&name.to_ascii_lowercase().as_str()) {
            bail!("'macports:{name}' is a port pseudo-name or operator, not a port");
        }
    }
    Ok(())
}

/// Run `port` through the configured elevator, or print it for a dry run.
fn run(port: &Path, args: Vec<String>, opts: &InstallOpts) -> Result<()> {
    let program = port.to_string_lossy();
    if opts.dry_run {
        miseprintln!("{}", sudo::argv(&program, &args).join(" "));
        return Ok(());
    }
    sudo::run(&program, &args, &[])
}

/// `port -N <action> <names>`. `-N` makes port fail instead of asking.
fn port_args(action: &str, pkgs: &[PackageRequest]) -> Vec<String> {
    ["-N", action]
        .into_iter()
        .map(str::to_string)
        .chain(pkgs.iter().map(|pkg| pkg.name.clone()))
        .collect()
}

/// Sync the ports tree, the metadata refresh that `--update` and upgrades ask
/// for. Unlike `port selfupdate`, it leaves MacPorts itself at its version.
fn sync(port: &Path, opts: &InstallOpts) -> Result<()> {
    run(port, vec!["-N".to_string(), "sync".to_string()], opts)
}

/// One line of `port installed`: `  name @version_revision+variants (active)`.
#[derive(Debug, PartialEq, Eq)]
struct InstalledPort<'a> {
    name: &'a str,
    /// `version_revision+variants`, as `port installed` prints it
    spec: &'a str,
    version: &'a str,
    revision: &'a str,
    active: bool,
}

impl InstalledPort<'_> {
    /// Whether a pin names this installation. port itself accepts the version
    /// alone or the whole `version_revision+variants`; `version_revision` is
    /// accepted too, since variants are not part of a version.
    fn matches(&self, pin: &str) -> bool {
        pin == self.spec
            || pin == self.version
            || pin == format!("{}_{}", self.version, self.revision)
    }
}

/// Split `version_revision+variants`. The revision is the first `_<digits>`
/// followed by a variant or the end: versions do not contain that, while
/// variant names such as `+perl5_34` may.
fn split_spec(spec: &str) -> Option<(&str, &str)> {
    spec.match_indices('_').find_map(|(index, _)| {
        let after = &spec[index + 1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let ends = after.len() == digits || after.as_bytes()[digits] == b'+';
        (digits > 0 && ends).then(|| (&spec[..index], &after[..digits]))
    })
}

fn parse_installed_line(line: &str) -> Option<InstalledPort<'_>> {
    let rest = line.strip_prefix("  ")?;
    let (name, rest) = rest.split_once(" @")?;
    let (spec, tail) = rest.split_once(' ').unwrap_or((rest, ""));
    let (version, revision) = split_spec(spec)?;
    Some(InstalledPort {
        name,
        spec,
        version,
        revision,
        active: tail.starts_with("(active)"),
    })
}

/// Reconcile `port installed` output with the requests. Only the active
/// installation counts: an inactive one is on disk but not in use, and
/// `port install` activates it. Versions are compared as strings, never
/// ordered.
fn parse_installed(output: &str, requests: &[PackageRequest]) -> Vec<PackageStatus> {
    let ports = output
        .lines()
        .filter_map(parse_installed_line)
        .filter(|port| port.active)
        .collect::<Vec<_>>();
    requests
        .iter()
        .map(|request| {
            // the registry matches port names case-insensitively
            let active = ports
                .iter()
                .find(|port| port.name.eq_ignore_ascii_case(&request.name));
            let state = match active {
                None => PackageState::Missing,
                Some(port)
                    if request
                        .version
                        .as_deref()
                        .is_none_or(|pin| port.matches(pin)) =>
                {
                    PackageState::Installed {
                        version: port.spec.to_string(),
                    }
                }
                Some(port) => PackageState::VersionMismatch {
                    installed: port.spec.to_string(),
                },
            };
            PackageStatus {
                request: request.clone(),
                state,
                display_name: None,
            }
        })
        .collect()
}

#[async_trait(?Send)]
impl SystemPackageManager for MacportsManager {
    fn name(&self) -> &str {
        "macports"
    }

    fn is_available(&self) -> bool {
        port_bin().is_some()
    }

    fn unavailable_reason(&self) -> String {
        if cfg!(target_os = "macos") {
            format!("port not found on PATH or at {DEFAULT_PORT}")
        } else {
            "only available on macos".to_string()
        }
    }

    /// A ports tree carries only each port's current Portfile, so a pin is
    /// checked against what is installed but cannot be installed.
    fn supports_version_pins(&self) -> bool {
        false
    }

    fn package_identity(&self, name: &str) -> Option<String> {
        Some(name.to_ascii_lowercase())
    }

    /// `port installed` reads the registry without root, the ports tree, or
    /// the network. It exits 0 for names that are not installed.
    async fn installed(&self, pkgs: &[PackageRequest]) -> Result<Vec<PackageStatus>> {
        if pkgs.is_empty() {
            return Ok(vec![]);
        }
        validate_names(pkgs)?;
        let port = require_port_bin()?;
        let output = tokio::process::Command::new(&port)
            .args(["-q", "installed"])
            .args(pkgs.iter().map(|pkg| &pkg.name))
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            bail!(
                "port installed failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(parse_installed(
            &String::from_utf8_lossy(&output.stdout),
            pkgs,
        ))
    }

    async fn install(&self, pkgs: &[PackageRequest], opts: &InstallOpts) -> Result<()> {
        if pkgs.is_empty() {
            return Ok(());
        }
        validate_names(pkgs)?;
        let port = require_port_bin()?;
        if opts.update {
            sync(&port, opts)?;
        }
        run(&port, port_args("install", pkgs), opts)
    }

    async fn upgrade(&self, pkgs: &[PackageRequest], opts: &InstallOpts) -> Result<()> {
        if pkgs.is_empty() {
            return Ok(());
        }
        validate_names(pkgs)?;
        let port = require_port_bin()?;
        sync(&port, opts)?;
        run(&port, port_args("upgrade", pkgs), opts)
    }

    fn supports_remove(&self) -> bool {
        true
    }

    /// Without `--follow-dependents`, port refuses to uninstall a port that
    /// installed ports depend on, and mise reports its error.
    async fn remove(&self, pkgs: &[PackageRequest], opts: &InstallOpts) -> Result<()> {
        if pkgs.is_empty() {
            return Ok(());
        }
        validate_names(pkgs)?;
        let port = require_port_bin()?;
        run(&port, port_args("uninstall", pkgs), opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(name: &str, version: Option<&str>) -> PackageRequest {
        PackageRequest {
            name: name.to_string(),
            version: version.map(str::to_string),
            tap_url: None,
            desired: super::super::PackageDesiredState::Present,
        }
    }

    #[test]
    fn splits_revision_from_version_and_variants() {
        assert_eq!(split_spec("2.47.0_0"), Some(("2.47.0", "0")));
        assert_eq!(
            split_spec("2.47.0_1+credential_osxkeychain+perl5_34"),
            Some(("2.47.0", "1"))
        );
        assert_eq!(split_spec("1.0+git_12+doc"), Some(("1.0+git", "12")));
        assert_eq!(split_spec("2024_beta_3"), Some(("2024_beta", "3")));
        assert_eq!(split_spec("1.0"), None);
    }

    #[test]
    fn parses_installed_lines() {
        assert_eq!(
            parse_installed_line("  git @2.47.0_0+perl5_34 (active)"),
            Some(InstalledPort {
                name: "git",
                spec: "2.47.0_0+perl5_34",
                version: "2.47.0",
                revision: "0",
                active: true,
            })
        );
        let inactive = parse_installed_line("  bc @1.07.1_0").unwrap();
        assert!(!inactive.active);
        assert_eq!(
            parse_installed_line("The following ports are currently installed:"),
            None
        );
        assert_eq!(
            parse_installed_line("None of the specified ports are installed."),
            None
        );
    }

    #[test]
    fn only_the_active_installation_counts() {
        let requests = [
            req("git", None),
            req("bc", None),
            req("missing", None),
            req("Git", Some("2.47.0")),
            req("git", Some("2.47.0_1")),
            req("git", Some("2.47.0_1+perl5_34")),
            req("git", Some("2.46.0")),
            req("git", Some("2.47")),
        ];
        let output = "The following ports are currently installed:\n  \
             bc @1.07.1_0\n  \
             git @2.46.0_0+perl5_34\n  \
             git @2.47.0_1+perl5_34 (active)\n";
        let statuses = parse_installed(output, &requests);
        for index in [0, 3, 4, 5] {
            assert_eq!(
                statuses[index].state,
                PackageState::Installed {
                    version: "2.47.0_1+perl5_34".to_string()
                },
                "{index}"
            );
        }
        // an inactive installation is not in use
        assert_eq!(statuses[1].state, PackageState::Missing);
        assert_eq!(statuses[2].state, PackageState::Missing);
        // pins match exactly, never as a prefix, and never an inactive version
        for index in [6, 7] {
            assert_eq!(
                statuses[index].state,
                PackageState::VersionMismatch {
                    installed: "2.47.0_1+perl5_34".to_string()
                },
                "{index}"
            );
        }
    }

    #[test]
    fn rejects_names_port_would_not_read_as_one_port() {
        for name in [
            "outdated",
            "Installed",
            "and",
            "-f",
            "+universal",
            "git@2.47",
            "name:git",
            "py*",
            "a b",
            "(git)",
            "",
        ] {
            assert!(validate_names(&[req(name, None)]).is_err(), "{name}");
        }
        for name in [
            "git",
            "py313-pip",
            "gtk+3",
            "libsigc++2",
            "p5.34-dbi",
            "xz_utils",
        ] {
            assert!(validate_names(&[req(name, None)]).is_ok(), "{name}");
        }
    }

    #[test]
    fn builds_noninteractive_commands() {
        assert_eq!(
            port_args("install", &[req("git", None), req("bc", None)]),
            ["-N", "install", "git", "bc"]
        );
    }
}
