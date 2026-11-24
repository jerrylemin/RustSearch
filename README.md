# RustSearch for Windows

RustSearch is a Rust implementation of the core browser experience in [Office Commun's Search](https://github.com/driceroland/Search/tree/d4a727c08e29161dad72f382ef101f78e72f8480). It embeds [Servo 0.8.0](https://github.com/servo/servo) to render websites and the local browser interface on Windows x64.

## Run

Open `dist\RustSearch.exe` on Windows x64. The release bundles Servo and its browser resources.

Build or run from a PowerShell terminal with Rust, the MSVC C++ build tools and Windows SDK, CMake, and a graphics driver with OpenGL support. The executable accepts an initial address, search query, or local HTML file path.

```powershell
cargo test --locked
cargo build --release --locked
```

Cargo writes `rustsearch.exe` under the configured target directory's `release` folder. To run directly from Cargo, use `cargo run --release --locked`.

## Browse

| Shortcut | Action |
| --- | --- |
| Ctrl+L | Focus the address and search field |
| Ctrl+T / Ctrl+W / Ctrl+Shift+T | Open, close, or reopen a tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Switch between tabs |
| Ctrl+K / Ctrl+1–9 | Search open tabs or jump to one |
| Ctrl+Shift+N | Open a private window, or a new tab within a private window |
| Alt+Left / Alt+Right / Ctrl+R | Go back, go forward, or reload |
| Ctrl+D / Ctrl+Shift+B | Toggle a bookmark or view bookmarks |
| Ctrl+H | View browsing history |
| Ctrl+F | Find text on the page |
| Ctrl+Shift+R | Toggle reading view |
| Ctrl+Shift+S | Move the tab bar between the top and left |

The browser menu contains pinning, split view, tracker exceptions, element hiding, and search engine settings. A split restores with the regular tabs that form it. Drag its divider or focus it and use the arrow keys. To hide an element on a regular website, choose **Hide an element**, click the element, and choose **Keep hidden**. **Reset hidden elements** removes the rules for that hostname. Element rules are reapplied after navigation completes. The tracker filter uses Search's list of known domains for third-party subresources and supports exceptions per hostname.

Regular tabs, bookmarks, history, hidden elements, and preferences still use `%LOCALAPPDATA%\RustSearch\session.json`. Servo keeps regular website data under the profile's `Servo` subdirectory. Existing Edge website data is separate, so sites may require signing in again. Each private window uses a separate process with temporary website storage. Private tabs stay out of the browser's session and history files. To use a separate profile, set `RUSTSEARCH_DATA_DIR` to an absolute directory before launching the application.

## Port coverage

Servo determines website compatibility. This build does not enable Servo's optional GStreamer support and excludes file downloads and picture-in-picture. Find searches for matches within rendered text nodes. Apple Keychain and Passwords integration, passkeys managed by Search, browser imports, the extension store and its compatibility shims, Spaces, custom murals, AppleScript, and Office Commun's signed updater are outside this build.

The upstream MIT notice is in [LICENSE](LICENSE).
