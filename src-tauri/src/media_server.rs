use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

#[derive(Clone)]
pub struct MediaServer {
    base_url: String,
    cache_dir: PathBuf,
}

impl MediaServer {
    pub fn start(cache_dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&cache_dir).map_err(|err| err.to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
        listener
            .set_nonblocking(false)
            .map_err(|err| err.to_string())?;
        let port = listener.local_addr().map_err(|err| err.to_string())?.port();
        let base_url = format!("http://127.0.0.1:{port}");
        let cache_dir_arc = Arc::new(cache_dir.clone());

        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let cache_dir = Arc::clone(&cache_dir_arc);
                thread::spawn(move || {
                    let _ = handle_connection(stream, &cache_dir);
                });
            }
        });

        Ok(Self { base_url, cache_dir })
    }

    pub fn media_url(&self, cache_path: &Path) -> Result<String, String> {
        let file_name = cache_path
            .file_name()
            .ok_or_else(|| "Downloaded video file is missing a name".to_string())?;
        let file_name = file_name
            .to_str()
            .ok_or_else(|| "Downloaded video file has an invalid name".to_string())?;
        if file_name.contains('/') || file_name.contains('\\') || file_name.contains("..") {
            return Err("Downloaded video file has an unsafe name".to_string());
        }

        let canonical = cache_path.canonicalize().map_err(|err| err.to_string())?;
        let cache_root = self.cache_dir.canonicalize().map_err(|err| err.to_string())?;
        if !canonical.starts_with(&cache_root) {
            return Err("Downloaded video path is outside the cache directory".to_string());
        }

        Ok(format!("{}/media/{file_name}", self.base_url))
    }
}

fn handle_connection(mut stream: TcpStream, cache_dir: &Path) -> std::io::Result<()> {
    let mut buffer = [0u8; 4096];
    let size = stream.read(&mut buffer)?;
    if size == 0 {
        return Ok(());
    }

    let request = std::str::from_utf8(&buffer[..size]).unwrap_or("");
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" {
        return write_response(&mut stream, "405 Method Not Allowed", &[], b"Method Not Allowed");
    }

    let Some(file_name) = target.strip_prefix("/media/") else {
        return write_response(&mut stream, "404 Not Found", &[], b"Not Found");
    };

    if file_name.is_empty()
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.contains("..")
    {
        return write_response(&mut stream, "403 Forbidden", &[], b"Forbidden");
    }

    let file_path = cache_dir.join(file_name);
    let file = match File::open(&file_path) {
        Ok(file) => file,
        Err(_) => return write_response(&mut stream, "404 Not Found", &[], b"Not Found"),
    };

    let metadata = file.metadata()?;
    let file_len = metadata.len();
    let mut range = (0, file_len.saturating_sub(1));

    for line in lines {
        if let Some(value) = line.strip_prefix("Range: ") {
            if let Some(parsed) = parse_range(value, file_len) {
                range = parsed;
            }
        }
        if line.is_empty() {
            break;
        }
    }

    let (start, end) = range;
    let content_len = end.saturating_sub(start).saturating_add(1);
    let mut file = file;
    let mut body = vec![0u8; content_len as usize];
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut body)?;

    let status = if start == 0 && end + 1 == file_len {
        "200 OK"
    } else {
        "206 Partial Content"
    };

    let mut headers = vec![
        "Content-Type: video/mp4".to_string(),
        format!("Content-Length: {content_len}"),
        "Accept-Ranges: bytes".to_string(),
        "Access-Control-Allow-Origin: *".to_string(),
    ];
    if status == "206 Partial Content" {
        headers.push(format!("Content-Range: bytes {start}-{end}/{file_len}"));
    }

    write_response(&mut stream, status, &headers, &body)
}

fn parse_range(value: &str, file_len: u64) -> Option<(u64, u64)> {
    let value = value.trim();
    let bytes = value.strip_prefix("bytes=")?;
    let (start_raw, end_raw) = bytes.split_once('-')?;
    let start = start_raw.parse().ok()?;
    let end = if end_raw.is_empty() {
        file_len.saturating_sub(1)
    } else {
        end_raw.parse().ok()?
    };
    if start >= file_len || end >= file_len || start > end {
        return None;
    }
    Some((start, end))
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    headers: &[String],
    body: &[u8],
) -> std::io::Result<()> {
    let mut response = format!("HTTP/1.1 {status}\r\n");
    for header in headers {
        response.push_str(header);
        response.push_str("\r\n");
    }
    response.push_str("Connection: close\r\n\r\n");
    stream.write_all(response.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn parses_byte_ranges() {
        assert_eq!(parse_range("bytes=0-1023", 5000), Some((0, 1023)));
        assert_eq!(parse_range("bytes=100-", 5000), Some((100, 4999)));
    }
}
