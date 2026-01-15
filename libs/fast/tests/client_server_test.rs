// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

// Tests are allowed to panic and use unwrap/expect
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Error;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use slog::{Drain, Logger, debug, error, info, o};
use tokio::net::TcpListener;

use fast_rpc::client;
use fast_rpc::protocol::{FastMessage, FastMessageId};
use fast_rpc::server;

fn echo_handler(
    msg: &FastMessage,
    mut response: Vec<FastMessage>,
    log: &Logger,
) -> Result<Vec<FastMessage>, Error> {
    debug!(log, "handling echo function request");
    response.push(FastMessage::data(msg.id, msg.data.clone()));
    Ok(response)
}

fn msg_handler(
    msg: &FastMessage,
    log: &Logger,
) -> Result<Vec<FastMessage>, Error> {
    let response: Vec<FastMessage> = vec![];

    match msg.data.m.name.as_str() {
        "echo" => echo_handler(msg, response, log),
        _ => Err(Error::other(format!(
            "Unsupported function: {}",
            msg.data.m.name
        ))),
    }
}

fn run_server(barrier: Arc<Barrier>) {
    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let root_log = Logger::root(
        Mutex::new(slog_term::FullFormat::new(plain).build()).fuse(),
        o!("build-id" => "0.1.0"),
    );

    let addr_str = "127.0.0.1:56652".to_string();
    match addr_str.parse::<SocketAddr>() {
        Ok(addr) => {
            // Use a tokio runtime for the server
            let rt = tokio::runtime::Runtime::new()
                .expect("failed to create runtime");
            rt.block_on(async {
                let listener = TcpListener::bind(&addr).await.expect("failed to bind");
                info!(root_log, "listening for fast requests"; "address" => addr);

                // Signal to the test that the server is ready
                barrier.wait();

                loop {
                    match listener.accept().await {
                        Ok((socket, _)) => {
                            let process_log = root_log.clone();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    server::handle_connection(socket, msg_handler, Some(&process_log)).await
                                {
                                    error!(process_log, "connection error"; "err" => %e);
                                }
                            });
                        }
                        Err(e) => {
                            error!(root_log, "failed to accept socket"; "err" => %e);
                        }
                    }
                }
            });
        }
        Err(e) => {
            eprintln!("error parsing address: {}", e);
        }
    }
}

fn assert_handler(expected_data_size: usize) -> impl Fn(&FastMessage) {
    move |msg| {
        let data: Vec<String> =
            serde_json::from_value(msg.data.d.clone()).unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].len(), expected_data_size);
    }
}

fn response_handler(
    data_size: usize,
) -> impl Fn(&FastMessage) -> Result<(), Error> {
    let handler = assert_handler(data_size);
    move |msg| {
        handler(msg);
        Ok(())
    }
}

#[test]
fn client_server_comms() {
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();
    let _h_server = thread::spawn(move || run_server(barrier_clone));

    barrier.wait();

    // Give the server a moment to fully start accepting connections
    thread::sleep(Duration::from_millis(100));

    let addr_str = "127.0.0.1:56652".to_string();
    let addr = addr_str.parse::<SocketAddr>().unwrap();

    let mut stream =
        TcpStream::connect(addr).expect("Failed to connect to server");

    (1..100).for_each(|x| {
        let data_size = x * 1000;
        let method = String::from("echo");
        let args_str = ["[\"", &"a".repeat(data_size), "\"]"].concat();
        let args: Value = serde_json::from_str(&args_str).unwrap();
        let handler = response_handler(data_size);
        let mut msg_id = FastMessageId::new();
        let result = client::send(method, args, &mut msg_id, &mut stream)
            .and_then(|_bytes_written| client::receive(&mut stream, handler));

        assert!(result.is_ok());
    });

    let shutdown_result = stream.shutdown(Shutdown::Both);

    assert!(shutdown_result.is_ok());
}
