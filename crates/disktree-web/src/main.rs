//! `disktree-web`: the disktree treemap, served to a browser.
//!
//! Same screens, same state machine, same removal guards as the desktop
//! app — the browser is only a terminal that draws frames and posts input
//! back. Bound to localhost by default: point an SSH forward at it, or pass
//! `--listen` and `--token` to expose it wider.

mod app;
mod mosaic;
mod palette;
mod render;
mod server;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use disktree_core::scan::ScanOptions;
use disktree_core::tree::Metric;

use app::Web;

/// What the command line asked for.
#[derive(Debug)]
struct Args {
    root: PathBuf,
    listen: SocketAddr,
    token: Option<String>,
    options: ScanOptions,
    depth: u32,
}

const USAGE: &str = "\
disktree-web — the disktree treemap in a browser, for remote machines

usage: disktree-web [OPTIONS] [PATH]

arguments:
  PATH              directory to scan (default: the home directory)

The page is the desktop app's twin: same treemap, same marks, same review
screen, same removal guards. Everything is decided server-side; the browser
draws what it is sent.

options:
  -a, --apparent-size   measure apparent length instead of allocated blocks
  -l, --follow-links    follow symlinks
  -H, --no-hidden       skip dotfiles and dot-directories
  -X, --cross-filesystems
                        also measure other disks, network shares and pseudo
                        filesystems mounted below PATH (off by default)
  -d, --depth N         how many levels to draw at once (1-6, default 3)
      --metric files    rank by file count instead of bytes
      --listen ADDR     bind address (default: 127.0.0.1:8737)
      --token TOKEN     require ?token=TOKEN on every request; also read from
                        DISKTREE_TOKEN. Set this before listening on anything
                        but localhost: anyone who can reach the page can mark
                        and remove files under PATH.
  -h, --help            show this help
";

fn main() -> Result<()> {
    let args = parse_args()?;
    let app = Web::new(args.root.clone(), args.options, args.depth);

    let url = match &args.token {
        Some(token) => format!("http://{}/?token={token}", args.listen),
        None => format!("http://{}", args.listen),
    };
    let exposed = !args.listen.ip().is_loopback();
    if exposed && args.token.is_none() {
        eprintln!(
            "disktree-web: warning: {} is reachable by other machines and \
             this page can remove files under {}. Restart with --token.",
            args.listen,
            args.root.display(),
        );
    }
    eprintln!("disktree-web: serving {} on {url}", args.root.display());

    server::serve(app, args.listen, args.token.as_deref());
}

fn parse_args() -> Result<Args> {
    let mut root: Option<PathBuf> = None;
    let mut listen = SocketAddr::from(([127, 0, 0, 1], 8737));
    let mut token = std::env::var("DISKTREE_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let mut options = ScanOptions::default();
    let mut depth = 3_u32;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-a" | "--apparent-size" => options.apparent_size = true,
            "-l" | "--follow-links" => options.follow_links = true,
            "-H" | "--no-hidden" => options.include_hidden = false,
            "-X" | "--cross-filesystems" => options.one_filesystem = false,
            "-d" | "--depth" => {
                let value = args.next().context("--depth wants a number")?;
                depth = value.parse::<u32>().with_context(|| {
                    format!("--depth: not a number: {value}")
                })?;
                if !(1..=6).contains(&depth) {
                    bail!("--depth must be between 1 and 6, got {depth}");
                }
            }
            "--metric" => {
                let value = args.next().context("--metric wants size|files")?;
                options.metric = match value.as_str() {
                    "size" => Metric::Bytes,
                    "files" => Metric::Files,
                    _ => bail!("--metric must be size or files, got {value}"),
                };
            }
            "--listen" => {
                let value = args.next().context("--listen wants host:port")?;
                listen = value.parse().with_context(|| {
                    format!("--listen: not host:port: {value}")
                })?;
            }
            "--token" => {
                let value = args.next().context("--token wants a value")?;
                if value.is_empty() {
                    bail!("--token must not be empty");
                }
                token = Some(value);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => {
                bail!("unknown option: {arg}\n\n{USAGE}");
            }
            _ => {
                if root.is_some() {
                    bail!("one PATH at a time: {arg}\n\n{USAGE}");
                }
                root = Some(PathBuf::from(arg));
            }
        }
    }

    let root = root
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .context("no PATH and no HOME to scan")?;
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot read {}", root.display()))?;
    if !root.is_dir() {
        bail!("not a directory: {}", root.display());
    }
    Ok(Args {
        root,
        listen,
        token,
        options,
        depth,
    })
}
