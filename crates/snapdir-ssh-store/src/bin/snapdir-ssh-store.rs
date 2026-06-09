//! The `ssh://` external-store binary: a thin shim into the library.

fn main() -> std::process::ExitCode {
    let code = snapdir_ssh_store::run(
        snapdir_ssh_store::Engine::Ssh,
        std::env::args_os(),
        std::io::stdin().lock(),
    );
    std::process::ExitCode::from(code)
}
