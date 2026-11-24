use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, net::Ipv4Addr, path::Path};
use url::{Host, Url};

pub const BLANK: &str = "about:blank";

#[derive(Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Page {
    pub url: String,
    pub title: String,
}

impl Page {
    pub fn label(&self) -> &str {
        if !self.title.is_empty() {
            &self.title
        } else if self.url == BLANK {
            "New tab"
        } else {
            &self.url
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct SavedTab {
    pub page: Page,
    pub pinned: bool,
}

#[derive(Clone, Copy, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    #[default]
    Google,
    DuckDuckGo,
    Bing,
    Brave,
    Ecosia,
    Startpage,
    Kagi,
    Qwant,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct State {
    #[serde(skip_serializing)]
    pub tabs: Vec<SavedTab>,
    pub selected: usize,
    pub split: Option<[usize; 2]>,
    pub split_percent: Option<u8>,
    pub history: Vec<Page>,
    pub bookmarks: Vec<Page>,
    pub engine: Engine,
    pub side_tabs: bool,
    pub tracker_exceptions: Vec<String>,
    pub hidden_elements: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
struct StoredPage<'a> {
    url: &'a str,
    title: &'a str,
}

#[derive(Serialize)]
struct StoredTab<'a> {
    page: StoredPage<'a>,
    pinned: bool,
}

#[derive(Serialize)]
struct SessionFile<'a> {
    tabs: Vec<StoredTab<'a>>,
    #[serde(flatten)]
    state: &'a State,
}

impl State {
    pub fn read(path: &Path) -> io::Result<Self> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        Ok(serde_json::from_reader(io::BufReader::new(file))?)
    }

    pub fn write<'a>(
        &self,
        path: &Path,
        tabs: impl Iterator<Item = &'a SavedTab>,
    ) -> io::Result<()> {
        let next = path.with_extension("tmp");
        let file = fs::File::create(&next)?;
        let tabs = tabs
            .map(|tab| StoredTab {
                page: if tab.page.url.starts_with("data:") {
                    StoredPage {
                        url: BLANK,
                        title: "",
                    }
                } else {
                    StoredPage {
                        url: &tab.page.url,
                        title: &tab.page.title,
                    }
                },
                pinned: tab.pinned,
            })
            .collect();
        serde_json::to_writer(&file, &SessionFile { tabs, state: self })?;
        file.sync_all()?;
        drop(file);
        fs::rename(next, path)
    }

    pub fn visit(&mut self, page: &Page) {
        self.history.retain(|item| item.url != page.url);
        self.history.insert(0, page.clone());
        self.history.truncate(1000);
    }
}

pub fn allowed(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| match url.scheme() {
        "http" | "https" => url.username().is_empty() && url.password().is_none(),
        "file" => url.host_str().is_none(),
        "about" => url.path() == "blank",
        "data" => true,
        _ => false,
    })
}

pub fn web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

pub fn address(input: &str, engine: Engine) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let path = Path::new(input);
    if path.is_absolute() && !input.starts_with("\\\\") && path.is_file() {
        let url = Url::from_file_path(path).ok()?;
        return allowed(url.as_str()).then(|| url.into());
    }
    let prefix = input.split(['/', '?', '#']).next().unwrap_or_default();
    let explicit = input.contains("://")
        || ["data:", "about:"].iter().any(|scheme| {
            input
                .get(..scheme.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        });
    if explicit {
        let mut url = Url::parse(input).ok()?;
        if url.host_str() == Some("0.0.0.0") {
            url.set_host(Some("localhost")).ok()?;
        }
        return allowed(url.as_str()).then(|| url.into());
    }
    if !input.contains(char::is_whitespace) && !prefix.contains('@') {
        let host = if let Some(bracket) = prefix.strip_prefix('[') {
            bracket
                .split_once(']')
                .map(|(ip, _)| ip)
                .unwrap_or_default()
        } else {
            prefix.split(':').next().unwrap_or_default()
        };
        let host = host.to_lowercase();
        let ipv4 = host.parse::<Ipv4Addr>().ok();
        let local = host == "localhost"
            || host.ends_with(".localhost")
            || host.ends_with(".local")
            || host == "::1"
            || ipv4.is_some_and(|ip| ip.is_private() || ip.is_loopback() || ip.is_unspecified());
        let domain = host.rsplit_once('.').is_some_and(|(_, tail)| {
            tail.chars().count() >= 2
                && (tail.chars().all(char::is_alphabetic) || tail.starts_with("xn--"))
                && host.split('.').all(|part| {
                    !part.is_empty()
                        && !part.starts_with('-')
                        && !part.ends_with('-')
                        && part.chars().all(|ch| ch.is_alphanumeric() || ch == '-')
                })
        });
        if local || domain || ipv4.is_some() || host.parse::<std::net::Ipv6Addr>().is_ok() {
            let scheme = if local { "http" } else { "https" };
            let mut url = Url::parse(&format!("{scheme}://{input}")).ok()?;
            if matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_unspecified()) {
                url.set_host(Some("localhost")).ok()?;
            }
            return Some(url.into());
        }
        if input.split_once(':').is_some() {
            return None;
        }
    }
    let (base, parameter) = match engine {
        Engine::Google => ("https://www.google.com/search", "q"),
        Engine::DuckDuckGo => ("https://duckduckgo.com/", "q"),
        Engine::Bing => ("https://www.bing.com/search", "q"),
        Engine::Brave => ("https://search.brave.com/search", "q"),
        Engine::Ecosia => ("https://www.ecosia.org/search", "q"),
        Engine::Startpage => ("https://www.startpage.com/sp/search", "query"),
        Engine::Kagi => ("https://kagi.com/search", "q"),
        Engine::Qwant => ("https://www.qwant.com/", "q"),
    };
    let mut url = Url::parse(base).ok()?;
    url.query_pairs_mut().append_pair(parameter, input);
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_or_search() {
        let cases = [
            ("example.com", Some("https://example.com/")),
            (
                "  LOCALHOST:8080/test  ",
                Some("http://localhost:8080/test"),
            ),
            ("0.0.0.0:3000", Some("http://localhost:3000/")),
            ("[::1]:3000", Some("http://[::1]:3000/")),
            ("192.168.1.2", Some("http://192.168.1.2/")),
            (
                "cà phê & Rust",
                Some("https://www.google.com/search?q=c%C3%A0+ph%C3%AA+%26+Rust"),
            ),
            ("1.2.3", Some("https://www.google.com/search?q=1.2.3")),
            ("javascript:alert(1)", None),
            ("https://user:secret@example.com", None),
            ("file://other-server/share", None),
            ("  ", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                address(input, Engine::Google).as_deref(),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn history_keeps_latest_title_and_visit_order() {
        let mut state = State::default();
        for (url, title) in [("a", "A"), ("b", "B"), ("a", "New A")] {
            state.visit(&Page {
                url: url.into(),
                title: title.into(),
            });
        }
        assert_eq!(
            state.history.iter().map(Page::label).collect::<Vec<_>>(),
            ["New A", "B"]
        );
    }

    #[test]
    fn session_saves_live_tabs_in_the_existing_format() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("rustsearch-{}-{stamp}.json", std::process::id()));
        let previous = serde_json::json!({
            "tabs": [
                {"page": {"url": "https://example.com/", "title": "Cà phê"}, "pinned": false},
                {"page": {"url": "data:text/html,private document", "title": "Local"}, "pinned": true}
            ],
            "selected": 1,
            "split": [0, 1],
            "split_percent": 40,
            "engine": "duck_duck_go",
            "history": [{"url": "https://example.com/", "title": "Cà phê"}],
            "bookmarks": [],
            "side_tabs": true,
            "tracker_exceptions": ["example.com"],
            "hidden_elements": {"example.com": [".banner"]}
        });
        fs::write(&path, previous.to_string()).unwrap();
        let mut state = State::read(&path).unwrap();
        let tabs = std::mem::take(&mut state.tabs);
        state.write(&path, tabs.iter()).unwrap();
        let output = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();

        let mut expected = previous;
        expected["tabs"][1]["page"] = serde_json::json!({"url": BLANK, "title": ""});
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            expected
        );
        assert_eq!(tabs[1].page.url, "data:text/html,private document");
    }
}
