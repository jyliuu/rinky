//! `rinky` binary.

fn main() {
    // args_os: `std::env::args()` panics on an argument that is not UTF-8 (Linux file
    // names are bytes). Such an argument is an error with exit status 1.
    let mut args = Vec::new();
    for (i, arg) in std::env::args_os().enumerate() {
        match arg.into_string() {
            Ok(a) => args.push(a),
            Err(raw) => {
                eprintln!("rinky: argument {i} is not valid UTF-8: {}", raw.to_string_lossy());
                std::process::exit(1);
            }
        }
    }
    // Release builds unwind (panic = "unwind"): a panic on this thread ends here, not in
    // the runtime's exit status 101. The hook already logged the panic and wrote the
    // crash file; this logs the exit line, shows the note and exits 134 so the launcher
    // starts the framework again.
    let code = match std::panic::catch_unwind(|| ink_app::cli::main(args)) {
        Ok(code) => code,
        Err(_) => ink_app::logging::finish_after_main_panic(),
    };
    std::process::exit(code);
}
