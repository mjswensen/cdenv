//! Controlled TLS smart-HTTP Git fixture for the credential release gate.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rustls::{ServerConfig, ServerConnection, StreamOwned};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn main() {
    if let Err(error) = serve() {
        eprintln!("credential HTTPS fixture failed: {error}");
        std::process::exit(1);
    }
}

fn serve() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let certificate = PathBuf::from(arguments.next().ok_or("missing certificate")?);
    let private_key = PathBuf::from(arguments.next().ok_or("missing private key")?);
    let project_root = PathBuf::from(arguments.next().ok_or("missing project root")?);
    let credential = PathBuf::from(arguments.next().ok_or("missing credential file")?);
    let ready = PathBuf::from(arguments.next().ok_or("missing ready file")?);
    if arguments.next().is_some() {
        return Err("unexpected argument".to_owned());
    }

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "cannot install TLS provider")?;
    let certificates = CertificateDer::pem_file_iter(&certificate)
        .map_err(|_| "cannot read certificate")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid certificate")?;
    let private_key =
        PrivateKeyDer::from_pem_file(&private_key).map_err(|_| "invalid private key")?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|_| "certificate and private key do not match")?;
    let listener = TcpListener::bind("0.0.0.0:0").map_err(|_| "cannot bind fixture")?;
    let port = listener
        .local_addr()
        .map_err(|_| "cannot inspect fixture address")?
        .port();
    fs::write(&ready, port.to_string()).map_err(|_| "cannot write ready file")?;
    let config = Arc::new(config);

    for stream in listener.incoming() {
        let stream = stream.map_err(|_| "fixture accept failed")?;
        let config = Arc::clone(&config);
        let project_root = project_root.clone();
        let credential = credential.clone();
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, config, &project_root, &credential) {
                eprintln!("credential HTTPS fixture request failed: {error}");
            }
        });
    }
    Ok(())
}

fn handle(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    project_root: &Path,
    credential: &Path,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| "cannot bound fixture reads")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| "cannot bound fixture writes")?;
    let connection = ServerConnection::new(config).map_err(|_| "TLS setup failed")?;
    let mut stream = StreamOwned::new(connection, stream);
    let request = read_request(&mut stream)?;
    let expected = expected_authorization(credential, &request.target)?;
    let authorized = request
        .headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("authorization") && value == &expected);
    if !authorized {
        stream
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"cdenv-fixture\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .map_err(|_| "cannot write authorization response")?;
        return Ok(());
    }

    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    let mut command = Command::new("git");
    command
        .arg("http-backend")
        .env_clear()
        .env("GIT_PROJECT_ROOT", project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("REQUEST_METHOD", &request.method)
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env("REMOTE_USER", "fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(content_type) = request.header("content-type") {
        command.env("CONTENT_TYPE", content_type);
    }
    let mut backend = command
        .spawn()
        .map_err(|_| "cannot start git http-backend")?;
    backend
        .stdin
        .take()
        .ok_or("missing backend stdin")?
        .write_all(&request.body)
        .map_err(|_| "cannot write backend request")?;
    let output = backend
        .wait_with_output()
        .map_err(|_| "cannot read git http-backend")?;
    write_backend_response(&mut stream, &output.stdout)
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn read_request(stream: &mut (impl Read + Write)) -> Result<Request, String> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err("request headers exceed fixture bound".to_owned());
        }
        let mut buffer = [0_u8; 4096];
        let read = stream
            .read(&mut buffer)
            .map_err(|_| "cannot read HTTPS request")?;
        if read == 0 {
            return Err("truncated HTTPS request".to_owned());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text =
        std::str::from_utf8(&bytes[..header_end - 4]).map_err(|_| "non-UTF-8 request headers")?;
    let mut lines = header_text.split("\r\n");
    let mut request_line = lines.next().ok_or("missing request line")?.split(' ');
    let method = request_line.next().ok_or("missing method")?.to_owned();
    let target = request_line.next().ok_or("missing target")?.to_owned();
    if request_line.next() != Some("HTTP/1.1") || request_line.next().is_some() {
        return Err("unsupported HTTP request line".to_owned());
    }
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').ok_or("malformed request header")?;
            Ok((name.to_owned(), value.trim().to_owned()))
        })
        .collect::<Result<Vec<_>, &str>>()
        .map_err(str::to_owned)?;
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map_or(Ok(0), |(_, value)| value.parse::<usize>())
        .map_err(|_| "invalid content length")?;
    if content_length > MAX_BODY_BYTES {
        return Err("request body exceeds fixture bound".to_owned());
    }
    let mut body = bytes[header_end..].to_vec();
    while body.len() < content_length {
        let mut buffer = [0_u8; 8192];
        let read = stream
            .read(&mut buffer)
            .map_err(|_| "cannot read HTTPS request body")?;
        if read == 0 {
            return Err("truncated HTTPS request body".to_owned());
        }
        body.extend_from_slice(&buffer[..read]);
    }
    body.truncate(content_length);
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}

fn expected_authorization(path: &Path, target: &str) -> Result<String, String> {
    let request_path = target
        .split_once('?')
        .map_or(target, |(path, _)| path)
        .trim_start_matches('/');
    let repository_end = request_path
        .find(".git")
        .map(|index| index + ".git".len())
        .ok_or("request does not name a Git repository")?;
    let repository = &request_path[..repository_end];
    let value = fs::read_to_string(path).map_err(|_| "cannot read fixture credential")?;
    let mut matching = value.lines().filter_map(|line| {
        let mut fields = line.split('\t');
        let candidate = fields.next()?;
        let username = fields.next()?;
        let password = fields.next()?;
        (candidate == repository && fields.next().is_none()).then_some((username, password))
    });
    let (username, password) = matching.next().ok_or("no fixture credential for path")?;
    if matching.next().is_some() {
        return Err("duplicate fixture credential path".to_owned());
    }
    Ok(format!(
        "Basic {}",
        base64(&format!("{username}:{password}"))
    ))
}

fn base64(value: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in value.as_bytes().chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(char::from(ALPHABET[((bits >> 18) & 63) as usize]));
        output.push(char::from(ALPHABET[((bits >> 12) & 63) as usize]));
        output.push(if chunk.len() > 1 {
            char::from(ALPHABET[((bits >> 6) & 63) as usize])
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            char::from(ALPHABET[(bits & 63) as usize])
        } else {
            '='
        });
    }
    output
}

fn write_backend_response(stream: &mut impl Write, output: &[u8]) -> Result<(), String> {
    let (headers, body) = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (&output[..index], &output[index + 4..]))
        .or_else(|| {
            output
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (&output[..index], &output[index + 2..]))
        })
        .ok_or("malformed git http-backend response")?;
    let headers = String::from_utf8(headers.to_vec()).map_err(|_| "invalid backend headers")?;
    let mut status = "200 OK";
    let mut forwarded = Vec::new();
    for line in headers.lines() {
        if let Some(value) = line.strip_prefix("Status: ") {
            status = value;
        } else {
            forwarded.push(line.trim_end_matches('\r'));
        }
    }
    write!(stream, "HTTP/1.1 {status}\r\n").map_err(|_| "cannot write response")?;
    for header in forwarded {
        write!(stream, "{header}\r\n").map_err(|_| "cannot write response header")?;
    }
    write!(
        stream,
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .map_err(|_| "cannot write response framing")?;
    stream
        .write_all(body)
        .map_err(|_| "cannot write response body".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_selects_the_repository_path_and_account() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let credentials = temporary.path().join("credentials");
        fs::write(
            &credentials,
            "team/one.git\talice\tone\nteam/two.git\tbob\ttwo\n",
        )
        .expect("credential fixture");

        assert_eq!(
            expected_authorization(
                &credentials,
                "/team/two.git/info/refs?service=git-upload-pack"
            )
            .expect("path credential"),
            format!("Basic {}", base64("bob:two"))
        );
    }

    #[test]
    fn authorization_rejects_an_unconfigured_repository_path() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let credentials = temporary.path().join("credentials");
        fs::write(&credentials, "team/one.git\talice\tone\n").expect("credential fixture");

        assert!(expected_authorization(&credentials, "/team/two.git/HEAD").is_err());
    }
}
