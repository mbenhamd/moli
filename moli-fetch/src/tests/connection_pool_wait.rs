use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener as StdTcpListener,
};

/// Each response is held longer than the connect timeout, so a request that
/// waits for one of the six per-host connections spends more than the connect
/// timeout before its own connection becomes available.
const RESPONSE_HOLD: Duration = Duration::from_millis(600);
const CONNECT_TIMEOUT_MS: u64 = 300;
const REQUESTS: usize = 2 * FetchConfig::DEFAULT_HTTP_MAX_HOST_CONNECTIONS as usize;

fn serve_held_keep_alive_responses(listener: StdTcpListener) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            return;
        };
        thread::spawn(move || {
            let mut writer = stream.try_clone().expect("test stream clones");
            let mut reader = BufReader::new(stream);
            loop {
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 {
                        return;
                    }
                    if header == "\r\n" {
                        break;
                    }
                }
                thread::sleep(RESPONSE_HOLD);
                if writer
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

#[tokio::test]
async fn same_host_requests_beyond_the_connection_cap_wait_without_spending_connect_timeout()
-> Result<()> {
    let listener = StdTcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?;
    thread::spawn(move || serve_held_keep_alive_responses(listener));

    let mut config = FetchConfig::default();
    config.set_http_proxy(Some(String::new()));
    config.set_request_timeout_ms(10_000);
    config.set_connect_timeout_ms(Some(CONNECT_TIMEOUT_MS));
    let client = FetchClient::new(&config, new_shared_browser_cookie_store());

    let mut requests = tokio::task::JoinSet::new();
    for index in 0..REQUESTS {
        let handle = client.clone();
        let request = Request::new(
            "GET",
            &format!("http://{address}/module-{index}.js"),
            None,
            Vec::new(),
        )?
        .with_single_hop();
        requests.spawn(async move { (index, handle.fetch(request).await) });
    }

    let mut failures = Vec::new();
    while let Some(joined) = requests.join_next().await {
        let (index, response) = joined?;
        match response {
            Ok(response) => assert_eq!(response.body_text(), "ok"),
            Err(error) => failures.push(format!("module-{index}: {error:#}")),
        }
    }
    assert!(
        failures.is_empty(),
        "requests queued behind the per-host connection cap failed:\n{}",
        failures.join("\n")
    );

    assert!(client.shutdown().is_clean());
    Ok(())
}
