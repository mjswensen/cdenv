//! Tiny editor-independent remote-server simulator used only by release fixtures.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::ExitCode;

fn main() -> ExitCode {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("editor-server-simulator: bind failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("editor-server-simulator: address failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("{address}");
    let (mut stream, _) = match listener.accept() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("editor-server-simulator: accept failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    if stream.write_all(b"CDENV_EDITOR_SIMULATOR/1\n").is_err() {
        return ExitCode::FAILURE;
    }
    let mut request = [0_u8; 5];
    if stream.read_exact(&mut request).is_err() || request != *b"PING\n" {
        return ExitCode::FAILURE;
    }
    if stream.write_all(b"PONG\n").is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
