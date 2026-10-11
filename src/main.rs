fn main() {
    let code = clauck::cli::run(std::env::args_os().collect());
    std::process::exit(code);
}
