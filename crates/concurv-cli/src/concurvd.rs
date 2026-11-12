//! `concurvd`: the concurv relay (RFC 0003). It stores the objects and the
//! authoritative published branch heads of one repository and serves them
//! to authenticated devices over TLS. Human output by default; `--json`
//! prints one JSON document in the envelope of `concurv --json`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use concurv_core::{ConcurvError, ErrorCode, Result};
use concurv_sync::{Relay, RelayOptions, Scope, Server, ServerOptions};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "concurvd",
    version,
    about = "concurvd — the concurv relay: authoritative published branches of one repository"
)]
struct Cli {
    /// Emit a single versioned JSON document instead of human output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a relay in a new or empty directory (prints its fingerprint).
    Init {
        /// Relay directory.
        dir: PathBuf,
    },
    /// Print the certificate fingerprint clients must pin.
    Fingerprint {
        /// Relay directory.
        dir: PathBuf,
    },
    /// Manage the devices allowed to connect.
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,
    },
    /// Serve the relay until the process is stopped.
    Serve {
        /// Relay directory.
        dir: PathBuf,
        /// Address to listen on (port 0: any free port).
        #[arg(long, default_value = "127.0.0.1:7461")]
        listen: String,
        /// Connections served at once.
        #[arg(long, default_value_t = 64)]
        max_connections: usize,
        /// Seconds a connection may stay idle.
        #[arg(long, default_value_t = 120)]
        idle_timeout: u64,
    },
}

#[derive(Subcommand)]
enum DeviceCmd {
    /// Register a device and print its token (shown once).
    Add {
        /// Relay directory.
        dir: PathBuf,
        /// Device name.
        name: String,
        /// Scopes: read, write, publish:* or publish:<branch> (repeatable).
        /// Default: read, write, publish:*.
        #[arg(long = "scope")]
        scopes: Vec<String>,
    },
    /// List devices.
    List {
        /// Relay directory.
        dir: PathBuf,
    },
    /// Revoke a device's token.
    Revoke {
        /// Relay directory.
        dir: PathBuf,
        /// Device name.
        name: String,
    },
}

impl Cmd {
    fn name(&self) -> &'static str {
        match self {
            Cmd::Init { .. } => "init",
            Cmd::Fingerprint { .. } => "fingerprint",
            Cmd::Device {
                cmd: DeviceCmd::Add { .. },
            } => "device add",
            Cmd::Device {
                cmd: DeviceCmd::List { .. },
            } => "device list",
            Cmd::Device {
                cmd: DeviceCmd::Revoke { .. },
            } => "device revoke",
            Cmd::Serve { .. } => "serve",
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let name = cli.cmd.name();
    let json = cli.json;
    let r = run(cli.cmd, json);
    let (data, human, err) = match r {
        Ok((d, h)) => (Some(d), h, None),
        Err(e) => (None, String::new(), Some(e)),
    };
    if json {
        let mut doc = json!({ "concurv_json_version": 1, "command": name, "ok": err.is_none() });
        if let Some(d) = data {
            doc["data"] = d;
        }
        if let Some(e) = &err {
            doc["error"] = json!({
                "code": e.code.as_str(),
                "message": e.message,
                "exit_code": e.code.exit_code(),
            });
        }
        println!("{doc}");
    } else {
        if !human.is_empty() {
            println!("{human}");
        }
        if let Some(e) = &err {
            eprintln!("error[{}]: {}", e.code, e.message);
        }
    }
    match err {
        None => ExitCode::SUCCESS,
        Some(e) => ExitCode::from(u8::try_from(e.code.exit_code()).unwrap_or(1)),
    }
}

fn open(dir: &std::path::Path) -> Result<Relay> {
    Relay::open(dir, RelayOptions::default())
}

fn run(cmd: Cmd, json: bool) -> Result<(Value, String)> {
    match cmd {
        Cmd::Init { dir } => {
            let o = Relay::init(&dir)?;
            Ok((
                json!({ "dir": dir.display().to_string(), "relay": o.relay, "fingerprint": o.fingerprint }),
                format!(
                    "Initialized relay {} in {}.\nFingerprint (give it to every client): {}\n\
                     Next: `concurvd device add {} <name>`, then `concurvd serve {}`.",
                    o.relay,
                    dir.display(),
                    o.fingerprint,
                    dir.display(),
                    dir.display()
                ),
            ))
        }
        Cmd::Fingerprint { dir } => {
            let fp = open(&dir)?.fingerprint()?;
            Ok((json!({ "fingerprint": fp }), fp))
        }
        Cmd::Device {
            cmd: DeviceCmd::Add { dir, name, scopes },
        } => {
            let relay = open(&dir)?;
            let scopes: Vec<Scope> = if scopes.is_empty() {
                vec![Scope::Read, Scope::Write, Scope::Publish(None)]
            } else {
                scopes
                    .iter()
                    .map(|s| Scope::parse(s))
                    .collect::<Result<_>>()?
            };
            let token = relay.add_device(&name, &scopes)?;
            let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
            Ok((
                json!({ "device": name, "scopes": scopes, "token": token }),
                format!(
                    "Added device {name} ({}).\nToken (shown once; store it in a file for `concurv remote add --token-file`):\n{token}",
                    scopes.join(", ")
                ),
            ))
        }
        Cmd::Device {
            cmd: DeviceCmd::List { dir },
        } => {
            let devices = open(&dir)?.devices()?;
            let mut human = String::new();
            let arr: Vec<Value> = devices
                .iter()
                .map(|d| {
                    human.push_str(&format!(
                        "{}\t{}{}\n",
                        d.name,
                        d.scopes.join(","),
                        if d.revoked { "\trevoked" } else { "" }
                    ));
                    json!({ "name": d.name, "scopes": d.scopes, "revoked": d.revoked, "created_ms": d.created_ms })
                })
                .collect();
            if arr.is_empty() {
                human.push_str("No devices (add one with `concurvd device add`).");
            }
            Ok((json!({ "devices": arr }), human.trim_end().to_owned()))
        }
        Cmd::Device {
            cmd: DeviceCmd::Revoke { dir, name },
        } => {
            open(&dir)?.revoke_device(&name)?;
            Ok((
                json!({ "device": name, "revoked": true }),
                format!("Revoked device {name}."),
            ))
        }
        Cmd::Serve {
            dir,
            listen,
            max_connections,
            idle_timeout,
        } => {
            let relay = open(&dir)?;
            let fp = relay.fingerprint()?;
            let server = Server::bind(
                relay,
                &listen,
                ServerOptions {
                    max_connections,
                    idle_timeout: Duration::from_secs(idle_timeout.max(1)),
                    ..ServerOptions::default()
                },
            )?;
            let addr = server.local_addr()?;
            // Announce the address before serving (scripts and tests wait
            // for this line).
            let line = if json {
                json!({ "concurv_json_version": 1, "command": "serve", "ok": true,
                        "data": { "listening": addr.to_string(), "fingerprint": fp } })
                .to_string()
            } else {
                format!("listening on {addr} (fingerprint {fp})")
            };
            let mut out = std::io::stdout();
            writeln!(out, "{line}")
                .and_then(|()| out.flush())
                .map_err(|e| ConcurvError::io(e, "write", "<stdout>"))?;
            server.run()?;
            Err(ConcurvError::new(
                ErrorCode::Network,
                "the listener stopped",
            ))
        }
    }
}
