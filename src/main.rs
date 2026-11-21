use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some("--version" | "-V") = std::env::args().nth(1).as_deref() {
        println!("{}", recalla::version_line());
        ExitCode::SUCCESS
    } else {
        eprintln!("{}\nusage: recalla --version", recalla::version_line());
        ExitCode::from(2)
    }
}
