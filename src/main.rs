//! hertrees: every git worktree of a project, its pull request, and agent
//! sessions, in one Herdr or tmux popup.

mod app;
mod cli;
mod client;
mod config;
mod fuzzy;
mod herdr;
mod host;
mod icons;
mod keys;
mod model;
mod mux;
mod opener;
mod paint;
mod par;
mod pr;
mod presets;
mod proc;
mod project;
mod pyjson;
mod pyos;
mod pystr;
mod render;
mod sequences;
mod session;
mod term;
mod tmux;
#[cfg(test)]
mod testutil;
mod textutil;
mod theme;
mod unicode_tables;
mod wordmark;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let code = cli::main(args);
    term::restore_console();
    std::process::exit(code);
}
