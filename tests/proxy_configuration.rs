use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};
use x::rcfile::RcFile;
use x_api::http::ProxyConfig;
use x_api::oauth1::{self, Token};

fn local_server(response: &str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let address = format!("http://{}", listener.local_addr().unwrap());
    let response = response.to_string();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "no request reached the local server"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|value| value == b"\r\n\r\n") {
            let read = stream
                .read(&mut buffer)
                .expect("request should be readable");
            assert!(read > 0, "request ended before headers");
            request.extend_from_slice(&buffer[..read]);
            assert!(request.len() < 16384, "request headers are too large");
        }
        stream
            .write_all(response.as_bytes())
            .expect("response should be written");
        String::from_utf8(request).expect("request should be UTF-8")
    });
    (address, handle)
}

fn rejected_proxy() -> (String, thread::JoinHandle<String>) {
    local_server("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

fn authorize_through_proxy(profile: &std::path::Path, override_proxy: Option<&str>) {
    let mut command = cargo_bin_cmd!("x");
    command.args([
        "authorize",
        "--display-uri",
        "--profile",
        profile.to_str().unwrap(),
    ]);
    if let Some(proxy) = override_proxy {
        command.args(["--proxy", proxy]);
    }
    command
        .env("T_AUTHORIZE_API_KEY", "test-key")
        .env("T_AUTHORIZE_API_SECRET", "test-secret")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "*")
        .write_stdin("\n")
        .timeout(Duration::from_secs(30))
        .assert()
        .code(1)
        .stderr(predicate::str::contains("No active credentials").not());
}

#[test]
fn cli_proxy_overrides_saved_proxy_direct_mode_and_environment() {
    for configuration in ["proxy: http://127.0.0.1:1", "no_proxy: true"] {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile.xrc");
        fs::write(
            &profile,
            format!("configuration:\n  {configuration}\nprofiles: {{}}\n"),
        )
        .unwrap();
        let (proxy, handle) = rejected_proxy();
        authorize_through_proxy(&profile, Some(&proxy));
        assert!(
            handle
                .join()
                .unwrap()
                .starts_with("CONNECT api.twitter.com:443 ")
        );
    }
}

#[test]
fn saved_application_proxy_is_used_for_authorization() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile.xrc");
    let (proxy, handle) = rejected_proxy();
    fs::write(
        &profile,
        format!("configuration:\n  proxy: {proxy}\nprofiles: {{}}\n"),
    )
    .unwrap();
    authorize_through_proxy(&profile, None);
    assert!(
        handle
            .join()
            .unwrap()
            .starts_with("CONNECT api.twitter.com:443 ")
    );
}

#[test]
fn api_backend_uses_application_proxy() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile.xrc");
    let (proxy, handle) = rejected_proxy();
    fs::write(&profile, format!("configuration:\n  default_profile: [test, key]\n  proxy: {proxy}\nprofiles:\n  test:\n    key:\n      username: test\n      consumer_key: key\n      consumer_secret: test-secret\n      token: test-token\n      secret: test-token-secret\n")).unwrap();
    cargo_bin_cmd!("x")
        .args(["whoami", "--profile", profile.to_str().unwrap()])
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "*")
        .timeout(Duration::from_secs(30))
        .assert()
        .code(1);
    assert!(
        handle
            .join()
            .unwrap()
            .starts_with("CONNECT api.twitter.com:443 ")
    );
}

#[test]
fn proxy_configuration_preserves_accounts_and_can_be_disabled_or_reset() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile.xrc");
    fs::write(&profile, "configuration:\n  default_profile: [test, key]\nprofiles:\n  test:\n    key:\n      username: test\n      consumer_key: key\n      consumer_secret: test-secret\n      token: test-token\n      secret: test-token-secret\n").unwrap();
    let original = RcFile::load(&profile).unwrap();
    for (value, expected) in [
        (
            "http://127.0.0.1:10808",
            ProxyConfig::Url("http://127.0.0.1:10808".to_string()),
        ),
        ("off", ProxyConfig::Direct),
        ("auto", ProxyConfig::Inherit),
    ] {
        cargo_bin_cmd!("x")
            .args([
                "set",
                "proxy",
                value,
                "--profile",
                profile.to_str().unwrap(),
            ])
            .assert()
            .success();
        let updated = RcFile::load(&profile).unwrap();
        assert_eq!(updated.proxy_config(), expected);
        assert_eq!(updated.active_profile(), original.active_profile());
        assert_eq!(
            serde_json::to_value(updated.profiles()).unwrap(),
            serde_json::to_value(original.profiles()).unwrap()
        );
    }
}

#[test]
fn direct_flag_overrides_invalid_saved_proxy() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile.xrc");
    fs::write(
        &profile,
        "configuration:\n  proxy: ftp://proxy.invalid\nprofiles: {}\n",
    )
    .unwrap();
    cargo_bin_cmd!("x")
        .args([
            "authorize",
            "--no-proxy",
            "--display-uri",
            "--profile",
            profile.to_str().unwrap(),
        ])
        .env_remove("T_AUTHORIZE_API_KEY")
        .env_remove("T_AUTHORIZE_API_SECRET")
        .write_stdin("\n")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Enter your API key:"))
        .stderr(predicate::str::contains("standard input closed"));
}

#[test]
fn invalid_proxy_does_not_modify_profile_or_expose_proxy_password() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile.xrc");
    let original = "configuration: {}\nprofiles: {}\n";
    fs::write(&profile, original).unwrap();
    cargo_bin_cmd!("x")
        .args([
            "set",
            "proxy",
            "ftp://user:test-password@proxy.invalid",
            "--profile",
            profile.to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Proxy URL must"))
        .stderr(predicate::str::contains("test-password").not());
    assert_eq!(fs::read_to_string(profile).unwrap(), original);
}

#[test]
fn proxy_and_direct_flags_are_mutually_exclusive() {
    cargo_bin_cmd!("x")
        .args(["version", "--proxy", "http://127.0.0.1:10808", "--no-proxy"])
        .assert()
        .code(2);
}

#[test]
fn socks_proxy_urls_are_supported() {
    for scheme in ["socks5", "socks5h"] {
        ProxyConfig::Url(format!("{scheme}://127.0.0.1:10808"))
            .client()
            .unwrap();
    }
}

#[test]
fn socks5_and_socks5h_route_requests_through_proxy() {
    for (scheme, host) in [
        ("socks5", "127.0.0.1"),
        ("socks5h", "proxy-dns.example.invalid"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no SOCKS connection received");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let mut greeting = [0; 2];
            stream.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0; greeting[1] as usize];
            stream.read_exact(&mut methods).unwrap();
            assert!(methods.contains(&0));
            stream.write_all(&[5, 0]).unwrap();

            let mut connect = [0; 4];
            stream.read_exact(&mut connect).unwrap();
            assert_eq!(&connect[..3], &[5, 1, 0]);
            let destination = match connect[3] {
                1 => {
                    let mut ip = [0; 4];
                    stream.read_exact(&mut ip).unwrap();
                    std::net::Ipv4Addr::from(ip).to_string()
                }
                3 => {
                    let mut length = [0; 1];
                    stream.read_exact(&mut length).unwrap();
                    let mut domain = vec![0; length[0] as usize];
                    stream.read_exact(&mut domain).unwrap();
                    String::from_utf8(domain).unwrap()
                }
                kind => panic!("unexpected SOCKS address type: {kind}"),
            };
            let mut port = [0; 2];
            stream.read_exact(&mut port).unwrap();
            assert_eq!(u16::from_be_bytes(port), 80);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                .unwrap();

            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|value| value == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                assert!(request.len() < 16384);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            (destination, String::from_utf8(request).unwrap())
        });

        let client = ProxyConfig::Url(format!("{scheme}://{address}"))
            .builder()
            .unwrap()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let body = client
            .get(format!("http://{host}/test"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert_eq!(body, "ok");
        let (destination, request) = handle.join().unwrap();
        assert_eq!(destination, host);
        assert!(request.starts_with("GET /test HTTP/1.1"));
    }
}

#[test]
fn direct_http_client_reaches_local_server() {
    if let Ok(address) = std::env::var("X_TEST_DIRECT_URL") {
        let response = ProxyConfig::Direct
            .client()
            .unwrap()
            .get(address)
            .send()
            .unwrap();
        assert_eq!(response.text().unwrap(), "ok");
        return;
    }
    let (address, handle) =
        local_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "direct_http_client_reaches_local_server"])
        .env("X_TEST_DIRECT_URL", format!("{address}/test"))
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(handle.join().unwrap().starts_with("GET /test HTTP/1.1"));
}

#[test]
fn oauth_get_and_post_use_the_supplied_proxy_client() {
    for method in ["GET", "POST"] {
        let (address, handle) =
            local_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let client = ProxyConfig::Url(address).client().unwrap();
        let consumer = Token::new("test-key", "test-secret");
        let body = if method == "GET" {
            oauth1::get_with_client(
                &client,
                "http://service.invalid/test",
                &consumer,
                None,
                None,
            )
        } else {
            oauth1::post_with_client(
                &client,
                "http://service.invalid/test",
                &consumer,
                None,
                None,
            )
        };
        assert_eq!(body.unwrap(), "ok");
        let request = handle.join().unwrap();
        assert!(request.starts_with(&format!("{method} http://service.invalid/test HTTP/1.1")));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: oauth")
        );
    }
}
