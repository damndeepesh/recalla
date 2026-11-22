use std::process::ExitCode;

fn main() -> ExitCode {
    let code = recalla::cli::run(
        std::env::args_os(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(code)
}
