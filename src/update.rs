//! Asking GitHub whether a newer release exists.
//!
//! Once per start - and only while the switch in the About window is on -
//! finnCrop asks the releases API for the newest published release and compares
//! its tag with the version it is running. Nothing is downloaded: a newer
//! release is a line in the window and a link, and the new exe is dropped over
//! the old one by hand, which is what the unversioned release name is for.
//!
//! What leaves the machine is one GET, with the repository's name in the path
//! and the program's own name as the user agent. Nothing about the machine, the
//! file that is open, or the person using it: no cookies, no identifiers, no
//! query string, no telemetry. A machine with no network, or an answer that is
//! not what the API documents, is simply "could not check" - the window never
//! waits for the answer, and never complains about it.

use std::ffi::c_void;

/// `https://api.github.com/repos/finnuen/finnCrop/releases/latest`, split into
/// the two halves the request needs.
const HOST: &str = "api.github.com";
const PATH: &str = "/repos/finnuen/finnCrop/releases/latest";

/// A published release that is newer than the program asking.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Release {
    /// The tag as published: `1.14`, without the `v` the release title carries.
    pub tag: String,
    /// The release page, where the notes and the download are.
    pub url: String,
}

/// What a check found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Checked {
    /// A newer release is published.
    Newer(Release),
    /// Nothing newer is published, so there is nothing to do.
    Current,
    /// The question could not be asked, or the answer did not make sense.
    Failed(String),
}

/// Asks GitHub and turns the answer into what the window shows.
///
/// Blocking, and meant to be called on a worker thread: it waits for the
/// network, which is exactly what the thread drawing the window must not do.
pub fn check(current: &str) -> Checked {
    let body = match fetch() {
        Ok(body) => body,
        Err(e) => return Checked::Failed(e),
    };
    match parse(&body, current) {
        Ok(Some(release)) => Checked::Newer(release),
        Ok(None) => Checked::Current,
        Err(e) => Checked::Failed(e),
    }
}

/// Reads `tag_name` and `html_url` out of the release the API answered with,
/// and says whether that tag is newer than `current`.
fn parse(body: &str, current: &str) -> Result<Option<Release>, String> {
    let answer: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("GitHub's answer was not JSON ({e})"))?;
    let tag = answer
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("GitHub's answer had no tag_name")?;
    let url = answer
        .get("html_url")
        .and_then(|v| v.as_str())
        .ok_or("GitHub's answer had no html_url")?;
    if !newer(tag, current) {
        return Ok(None);
    }
    Ok(Some(Release { tag: tag.trim_start_matches(['v', 'V']).to_string(), url: url.to_string() }))
}

/// Whether `tag` names a later version than `current`. Both are the digits and
/// dots this project publishes - `1.14`, `1.14.0`, `v1.14` - compared part by
/// part, so `1.9` is older than `1.13` rather than merely shorter.
fn newer(tag: &str, current: &str) -> bool {
    let mut a = parts(tag);
    let mut b = parts(current);
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

/// `1.13` as `[1, 13]`. Anything that is not a number counts as zero, which is
/// what a tag this project would never publish deserves.
fn parts(v: &str) -> Vec<u32> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split('.')
        .map(|p| p.trim().parse().unwrap_or(0))
        .collect()
}

/// Opens an address in the browser the machine already has. Windows' own shell
/// does it, so there is no browser to know about and nothing to install.
pub fn open_in_browser(url: &str) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        /// What a double-click on a link does.
        const SW_SHOWNORMAL: i32 = 1;
        let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
        let addr: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            addr.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

#[cfg(windows)]
fn fetch() -> Result<String, String> {
    winhttp::get(HOST, PATH)
}

#[cfg(not(windows))]
fn fetch() -> Result<String, String> {
    Err("the update check needs Windows".to_string())
}

/// One HTTPS GET, through the HTTP stack Windows already has. WinHTTP brings
/// its own TLS, so the program carries no certificate code of its own and links
/// nothing it would have to ship.
#[cfg(windows)]
mod winhttp {
    use super::*;
    use windows_sys::Win32::Networking::WinHttp::*;

    /// How long the question may take, in milliseconds: long enough for a slow
    /// connection, short enough that a worker thread is not left waiting.
    const RESOLVE_MS: i32 = 4000;
    const CONNECT_MS: i32 = 4000;
    const SEND_MS: i32 = 4000;
    const RECEIVE_MS: i32 = 8000;

    /// A release is a few kilobytes. Anything larger is not an answer to this
    /// question, and is not read to the end.
    const MAX_BODY: usize = 256 * 1024;

    /// WinHTTP's handles must be closed, including on the paths that return
    /// early, so each one is held in a guard that closes it when dropped.
    struct Handle(*mut c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn get(host: &str, path: &str) -> Result<String, String> {
        let agent = wide("finnCrop");
        unsafe {
            let session = Handle(WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            ));
            if session.0.is_null() {
                return Err("WinHTTP could not start".to_string());
            }
            WinHttpSetTimeouts(session.0, RESOLVE_MS, CONNECT_MS, SEND_MS, RECEIVE_MS);

            let host_w = wide(host);
            let connect = Handle(WinHttpConnect(session.0, host_w.as_ptr(), 443, 0));
            if connect.0.is_null() {
                return Err(format!("could not reach {host}"));
            }

            let verb = wide("GET");
            let path_w = wide(path);
            let request = Handle(WinHttpOpenRequest(
                connect.0,
                verb.as_ptr(),
                path_w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if request.0.is_null() {
                return Err("could not open the request".to_string());
            }

            // GitHub asks every caller to name itself and answers 403 to one
            // that does not. The rest asks for the API's own JSON.
            let headers = wide("User-Agent: finnCrop\r\nAccept: application/vnd.github+json\r\n");
            // A length of -1 means the header string is null-terminated.
            if WinHttpSendRequest(request.0, headers.as_ptr(), u32::MAX, std::ptr::null(), 0, 0, 0) == 0 {
                return Err("could not send the request".to_string());
            }
            if WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
                return Err("GitHub did not answer".to_string());
            }

            let mut status: u32 = 0;
            let mut len = std::mem::size_of::<u32>() as u32;
            let asked = WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                &mut status as *mut u32 as *mut c_void,
                &mut len,
                std::ptr::null_mut(),
            );
            if asked == 0 {
                return Err("GitHub's answer had no status".to_string());
            }
            if status != 200 {
                return Err(format!("GitHub answered {status}"));
            }

            let mut body = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let mut got: u32 = 0;
                let read = WinHttpReadData(
                    request.0,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len() as u32,
                    &mut got,
                );
                if read == 0 || got == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..got as usize]);
                if body.len() > MAX_BODY {
                    return Err("GitHub's answer was too long".to_string());
                }
            }
            String::from_utf8(body).map_err(|_| "GitHub's answer was not text".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_tag_is_newer_and_an_older_one_is_not() {
        assert!(newer("1.14", "1.13"));
        assert!(!newer("1.13", "1.13"), "the running version is not an update");
        // Compared as numbers, not as text: 1.9 is older than 1.13.
        assert!(!newer("1.9", "1.13"));
        assert!(newer("1.13", "1.9"));
        assert!(newer("2.0", "1.99"));
        // A tag written with a v, or with the trailing part Cargo insists on,
        // is the same version.
        assert!(newer("v1.14", "1.13"));
        assert!(!newer("1.14.0", "1.14"));
        assert!(!newer("1.14", "1.14.0"));
        assert!(newer("1.13.1", "1.13"));
        // Nothing a release would be tagged with, and not an update.
        assert!(!newer("latest", "1.13"));
        assert!(!newer("", "1.13"));
    }

    #[test]
    fn the_answer_is_read_from_the_release_the_api_returned() {
        let answer = r#"{
            "tag_name": "1.14",
            "html_url": "https://github.com/finnuen/finnCrop/releases/tag/1.14",
            "draft": false,
            "prerelease": false
        }"#;
        assert_eq!(
            parse(answer, "1.13"),
            Ok(Some(Release {
                tag: "1.14".to_string(),
                url: "https://github.com/finnuen/finnCrop/releases/tag/1.14".to_string(),
            }))
        );
        // The same release, once it is the one running: nothing to report.
        assert_eq!(parse(answer, "1.14"), Ok(None));
        assert_eq!(parse(answer, "1.15"), Ok(None));
    }

    #[test]
    fn an_answer_that_makes_no_sense_is_an_error_not_a_release() {
        assert!(parse("<html>not the api</html>", "1.13").is_err());
        assert!(parse(r#"{"html_url":"https://example.com"}"#, "1.13").is_err());
        assert!(parse(r#"{"tag_name":"1.14"}"#, "1.13").is_err());
        assert!(parse("", "1.13").is_err());
    }
}
