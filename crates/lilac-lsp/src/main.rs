//! `lilac-lsp`: the Lilac language server binary.

fn main() {
    let (connection, io_threads) = lsp_server::Connection::stdio();
    let code = match lilac_lsp::run(connection) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("lilac-lsp: {error}");
            1
        }
    };
    if let Err(error) = io_threads.join() {
        eprintln!("lilac-lsp: io thread: {error}");
    }
    std::process::exit(code);
}
