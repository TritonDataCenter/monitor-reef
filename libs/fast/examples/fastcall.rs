// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

// Examples are CLI tools where panicking on errors is acceptable
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Error;
use std::net::{SocketAddr, TcpStream};
use std::process;

use clap::Parser;
use serde_json::Value;

use fast_rpc::client;
use fast_rpc::protocol::{FastMessage, FastMessageId};

fn parse_json(s: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(s)
}

#[derive(Parser, Debug)]
#[command(
    name = "fastcall",
    version,
    about = "Command-line tool for making a node-fast RPC method call"
)]
struct Args {
    /// DNS name or IP address for remote server
    #[arg(short = 'H', long, default_value = "127.0.0.1")]
    host: String,

    /// TCP port for remote server
    #[arg(short, long, default_value_t = 2030)]
    port: u16,

    /// Name of remote RPC method call
    #[arg(short, long)]
    method: String,

    /// JSON-encoded arguments for RPC method call
    #[arg(long, value_parser = parse_json)]
    args: Value,

    /// Abandon connection immediately after sending
    #[arg(short, long = "abandon-immediately")]
    abandon: bool,

    /// Leave connection open after receiving response
    #[arg(short = 'c', long = "leave-conn-open")]
    leave_open: bool,
}

fn stdout_handler(msg: &FastMessage) {
    println!("{}", msg.data.d);
}

fn response_handler(msg: &FastMessage) -> Result<(), Error> {
    match msg.data.m.name.as_str() {
        "date" | "echo" | "yes" | "getobject" | "putobject" => {
            stdout_handler(msg)
        }
        _ => println!("Received {} response", msg.data.m.name),
    }

    Ok(())
}

fn main() {
    let args = Args::parse();

    let addr_str = format!("{}:{}", args.host, args.port);
    let addr = addr_str.parse::<SocketAddr>().unwrap_or_else(|e| {
        eprintln!(
            "Failed to parse host and port as valid socket address: {}",
            e
        );
        process::exit(1)
    });

    let mut stream = TcpStream::connect(addr).unwrap_or_else(|e| {
        eprintln!("Failed to connect to server: {}", e);
        process::exit(1)
    });

    let mut msg_id = FastMessageId::new();

    let result = client::send(args.method, args.args, &mut msg_id, &mut stream)
        .and_then(|_bytes_written| {
            client::receive(&mut stream, response_handler)
        });

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }
}
