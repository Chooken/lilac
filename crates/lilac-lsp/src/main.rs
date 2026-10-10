//! `lilac-lsp`: the Lilac language server binary.

fn main() {
    let (connection, io_threads) = lsp_server::Connection::stdio();
    // The server drops (and the worker builds) whole syntax trees, which
    // recurse as deep as the program nests.
    let server = std::thread::Builder::new()
        .name("lilac-lsp".to_owned())
        .stack_size(lilac_driver::COMPILE_STACK_BYTES)
        .spawn(move || lilac_lsp::run(connection))
        .expect("spawning the server thread");
    let code = match server.join() {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("lilac-lsp: {error}");
            1
        }
        // The panic message is already printed.
        Err(_) => 101,
    };
    if let Err(error) = io_threads.join() {
        eprintln!("lilac-lsp: io thread: {error}");
    }
    std::process::exit(code);
}
