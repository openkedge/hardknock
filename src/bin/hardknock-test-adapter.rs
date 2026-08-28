// SPDX-License-Identifier: Apache-2.0
//! Model-free JSONL conformance driver. It communicates only through the Bridge.
use hardknock::bridge::{protocol::AgentEvent, transport::BridgeClient};
use std::io::{self, BufRead};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("HARDKNOCK_HOME").ok_or("Set HARDKNOCK_HOME")?;
    let mut client = BridgeClient::new(std::path::Path::new(&home));
    client.timeout = std::time::Duration::from_secs(5);
    let mut stdin = io::stdin().lock();
    loop {
        let mut line = Vec::new();
        loop {
            let buffer = stdin.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let count = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buffer.len(), |i| i + 1);
            if line.len() + count > hardknock::bridge::protocol::MAX_EVENT_BYTES {
                return Err("Event too large".into());
            }
            let done = buffer[count - 1] == b'\n';
            line.extend_from_slice(&buffer[..count]);
            stdin.consume(count);
            if done {
                break;
            }
        }
        if line.is_empty() {
            break;
        }
        let event: AgentEvent = serde_json::from_slice(&line)?;
        println!("{}", client.request(event).await?);
    }
    Ok(())
}
