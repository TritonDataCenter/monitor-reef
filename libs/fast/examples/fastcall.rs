// Copyright 2020 Joyent, Inc.

use std::io::Error;
use std::net::{SocketAddr, TcpStream};
use std::process;

use clap::{Arg, ArgMatches, Command, crate_version};
use serde_json::Value;

use fast_rpc::client;
use fast_rpc::protocol::{FastMessage, FastMessageId};

static APP: &str = "fastcall";
static DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u32 = 2030;

pub fn parse_opts(app: &'static str) -> ArgMatches {
    Command::new(app)
        .about("Command-line tool for making a node-fast RPC method call")
        .version(crate_version!())
        .arg(
            Arg::new("host")
                .help("DNS name or IP address for remote server")
                .long("host")
                .short('h')
                .required(false),
        )
        .arg(
            Arg::new("port")
                .help("TCP port for remote server (Default: 2030)")
                .long("port")
                .short('p')
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            Arg::new("method")
                .help("Name of remote RPC method call")
                .long("method")
                .short('m')
                .required(true),
        )
        .arg(
            Arg::new("args")
                .help("JSON-encoded arguments for RPC method call")
                .long("args")
                .required(true),
        )
        .arg(
            Arg::new("abandon")
                .long("abandon-immediately")
                .short('a')
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("leave_open")
                .long("leave-conn-open")
                .short('c')
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches()
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
    let matches = parse_opts(APP);
    let host = matches
        .get_one::<String>("host")
        .map(|s| s.as_str())
        .unwrap_or(DEFAULT_HOST);
    let port = matches
        .get_one::<u32>("port")
        .copied()
        .unwrap_or(DEFAULT_PORT);
    let addr = format!("{}:{}", host, port)
        .parse::<SocketAddr>()
        .unwrap_or_else(|e| {
            eprintln!(
                "Failed to parse host and port as valid socket address: \
                 {}",
                e
            );
            process::exit(1)
        });
    let method =
        matches
            .get_one::<String>("method")
            .cloned()
            .unwrap_or_else(|| {
                eprintln!("Failed to parse method argument as String");
                process::exit(1)
            });
    let args_str = matches.get_one::<String>("args").unwrap_or_else(|| {
        eprintln!("Failed to get args argument");
        process::exit(1)
    });
    let args: Value = serde_json::from_str(args_str).unwrap_or_else(|e| {
        eprintln!("Failed to parse args as JSON: {}", e);
        process::exit(1)
    });

    let mut stream = TcpStream::connect(addr).unwrap_or_else(|e| {
        eprintln!("Failed to connect to server: {}", e);
        process::exit(1)
    });

    let mut msg_id = FastMessageId::new();

    let result = client::send(method, args, &mut msg_id, &mut stream).and_then(
        |_bytes_written| client::receive(&mut stream, response_handler),
    );

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }
}
