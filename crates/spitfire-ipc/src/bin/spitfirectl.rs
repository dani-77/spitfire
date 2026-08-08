//! `spitfirectl` — a thin CLI client for spitfire's control socket, in the
//! spirit of `niri msg`/`hyprctl`.

use spitfire_ipc::{default_socket_path, send_request, Request};

fn usage() -> ! {
    eprintln!(
        "USAGE: spitfirectl <command>\n\
         \n\
         Commands:\n\
         \x20 reload                 reload config.lua without restarting\n\
         \x20 quit                   quit the compositor\n\
         \x20 layout <mode>          tile | floating | fibonacci | monocle\n\
         \x20 workspace focus <n>    switch to workspace n (1-based)\n\
         \x20 list-windows           list mapped windows as JSON\n\
         \x20 list-outputs           list outputs as JSON\n\
         \x20 list-workspaces        list workspaces as JSON\n"
    );
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let request = match args.next().as_deref() {
        Some("reload") => Request::ReloadConfig,
        Some("quit") => Request::Quit,
        Some("layout") => {
            let Some(mode) = args.next() else {
                eprintln!("spitfirectl layout: missing <mode>");
                usage();
            };
            Request::SetLayout { mode }
        }
        Some("workspace") => match args.next().as_deref() {
            Some("focus") => {
                let Some(index) = args.next().and_then(|s| s.parse().ok()) else {
                    eprintln!("spitfirectl workspace focus: missing/invalid <n>");
                    usage();
                };
                Request::FocusWorkspace { index }
            }
            _ => {
                eprintln!("spitfirectl workspace: expected 'focus <n>'");
                usage();
            }
        },
        Some("list-windows") => Request::ListWindows,
        Some("list-outputs") => Request::ListOutputs,
        Some("list-workspaces") => Request::ListWorkspaces,
        Some(other) => {
            eprintln!("spitfirectl: unknown command '{other}'");
            usage();
        }
        None => usage(),
    };

    let path = default_socket_path();
    let response = match send_request(&path, &request) {
        Ok(resp) => resp,
        Err(err) => {
            eprintln!(
                "spitfirectl: couldn't reach spitfire at {}: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    };

    if !response.ok {
        eprintln!(
            "spitfirectl: {}",
            response.error.as_deref().unwrap_or("unknown error")
        );
        std::process::exit(1);
    }

    if let Some(data) = response.data {
        match serde_json::to_string_pretty(&data) {
            Ok(pretty) => println!("{pretty}"),
            Err(_) => println!("{data}"),
        }
    }
}
