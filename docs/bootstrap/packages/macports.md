---
description: "Install and remove MacPorts ports with port from mise.toml."
---

# MacPorts ports (macports)

The `macports` manager installs and removes [MacPorts](https://www.macports.org/)
ports with `port`. It uses [sudo](/bootstrap/packages/#sudo) when mise is not
running as root.

```toml
[bootstrap.packages]
"macports:git" = "latest"
"macports:ripgrep" = "latest"
"macports:bc" = { state = "absent" }
```

```sh
mise bootstrap packages apply --manager macports --dry-run
mise bootstrap packages apply --manager macports
```

## Prerequisites

Install MacPorts first. The manager is available on macOS when `port` is on
`PATH` or at `/opt/local/bin/port`, where the MacPorts installer puts it. On
other machines, its entries show as
[`skipped`](/bootstrap/packages/#choose-platforms), so one config can hold
packages for several platforms.

mise does not add `/opt/local/bin` to `PATH`. The MacPorts installer adds it to
your shell profile; a new terminal finds the ports mise installs.

### Install MacPorts during bootstrap

`mise bootstrap` runs the
[`pre-packages` hook](/bootstrap.html#hooks) before it installs packages, so a
hook can install MacPorts on a new Mac:

```toml
[bootstrap.hooks.pre-packages]
description = "Install MacPorts if it is missing"
run = '''
[ "$(uname)" = Darwin ] || exit 0
[ -x /opt/local/bin/port ] && exit 0
xcode-select -p >/dev/null 2>&1 || {
  echo "Install the Xcode Command Line Tools first: xcode-select --install" >&2
  exit 1
}
version=2.12.7
case "$(sw_vers -productVersion | cut -d. -f1)" in
  27) release=27-GoldenGate ;;
  26) release=26-Tahoe ;;
  15) release=15-Sequoia ;;
  14) release=14-Sonoma ;;
  *) echo "Add this macOS release to the MacPorts hook" >&2; exit 1 ;;
esac
dir="$(mktemp -d)"
curl -fsSL -o "$dir/MacPorts.pkg" \
  "https://github.com/macports/macports-base/releases/download/v$version/MacPorts-$version-$release.pkg"
sudo installer -pkg "$dir/MacPorts.pkg" -target /
'''
```

Hooks run every time, so the script exits early once MacPorts is installed. Hooks
run only with the full `mise bootstrap`: `mise bootstrap packages apply`,
`--no-hooks`, and [safe mode](/security.html#safe-mode) skip them, and the
`macports:` entries then show as skipped until MacPorts is installed.

## Package names

Use the port name, such as `ripgrep` or `py313-pip`. MacPorts matches port names
without regard to case, so `macports:Git` and `macports:git` are the same port.

A name is one port: mise rejects names that `port` would read as something
else, such as variants (`+universal`), version selectors (`git@2.47.0`),
globs, field selectors (`name:git`), and pseudo-ports such as `outdated` or
`installed`.

mise installs a port's default variants and does not pass variants to `port`.

## Version pins

A ports tree has only each port's current Portfile, so mise cannot install a
pinned version. A pin is checked against the active installation, and
`apply` and `upgrade` skip a pinned entry with a warning instead of failing the
rest of the batch:

```toml
[bootstrap.packages]
"macports:git" = "2.47.0" # reported as a mismatch if another version is active
```

A pin can be the version (`2.47.0`), the version and revision (`2.47.0_1`), or
the whole installed spec with variants (`2.47.0_1+perl5_34`). It must match
exactly; `2.47` does not match `2.47.0`.

## Active and inactive ports

MacPorts can keep several versions of a port installed with one of them active.
mise reports only the active one. A port that is installed but inactive counts
as missing, and `apply` runs `port install`, which activates it or installs the
current version.

## What mise runs

| Operation                       | Command                                                  |
| ------------------------------- | -------------------------------------------------------- |
| Check installed state (no sudo) | `port -q installed <ports>`                              |
| Install                         | `port -N install <ports>`                                |
| `apply --update`                | `port -N sync` first                                     |
| Upgrade                         | `port -N sync`, then `port -N upgrade <installed ports>` |
| Remove                          | `port -N uninstall <ports>`                              |

`-N` makes `port` fail instead of asking a question. mise runs `port sync`,
which updates the ports tree, and not `port selfupdate`, which also upgrades
MacPorts itself; run `sudo port selfupdate` yourself when MacPorts reports a new
release.

## Remove packages

```toml
[bootstrap.packages]
"macports:ripgrep" = { state = "absent" }
```

`apply` removes an installed port with `port -N uninstall`. An entry that is
already absent needs nothing. mise does not pass `--follow-dependents`, so
MacPorts refuses to remove a port that other installed ports depend on, and mise
reports its error. Dependencies that MacPorts installed for a removed port stay
installed; `sudo port uninstall leaves` removes them.
