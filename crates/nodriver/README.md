# nodriver (Rust)

Chrome/Edge automation over the DevTools Protocol without chromedriver or Selenium.
A Rust port of [nodriver](https://github.com/ultrafunkamsterdam/nodriver).

```rust
use nodriver::{Browser, Config};

#[tokio::main]
async fn main() -> nodriver::Result<()> {
    let browser = Browser::launch(Config::new().lang("en-US")).await?;
    let tab = browser.get("https://example.com", false).await?;
    println!("{}", tab.title().await?);
    tab.find("More information", true).await?.mouse_click().await?;
    browser.close().await?;
    Ok(())
}
```

## Mapping from the Python package

All methods are `async` and return `nodriver::Result`.

### Browser and config

| Python | Rust |
|---|---|
| `uc.start(...)`, `Browser.create(config)` | `Browser::launch(Config::new()...)` |
| `uc.start(host=..., port=...)` | `Browser::connect(host, port)` |
| `Config(user_data_dir, headless, browser_executable_path, browser_args, sandbox, lang, host, port, expert)` | `Config::new().user_data_dir(..).headless(..).executable(..).arg(..).sandbox(..).lang(..).port(..).expert(..)` |
| `config.add_extension(path)` / `config.add_argument(arg)` | `Config::extension(dir)` / `Config::arg(arg)` |
| `browser.get(url, new_tab=...)` | `browser.get(url, new_tab)` |
| `browser.tabs`, `browser.main_tab`, `browser.targets`, `browser.update_targets()` | `browser.tabs()`, `browser.main_tab()`, `browser.targets()` (each call reads the current targets) |
| `browser.cookies.get_all()` / `set_all(cookies)` / `clear()` | `browser.cookies().get_all()` / `set_all(cookies)` / `clear()` |
| `browser.cookies.save(file, pattern)` / `load(file, pattern)` | `browser.cookies().save(path)` / `load(path)`; `save_filtered(path, \|c\| ..)` / `load_filtered(path, \|c\| ..)` |
| `browser.grant_all_permissions()` | `browser.grant_all_permissions()` (returns the types granted) |
| `browser.create_context(url, proxy_server=..., proxy_bypass_list=...)` | `browser.create_context(url, proxy_server, proxy_bypass_list)` |
| `browser.wait(t)`, `browser.sleep(t)`, `tab.wait(t)`, `tab.sleep(t)` | `tokio::time::sleep(..)` |
| `util.get_browser_version_info()` | `browser.version()` |
| `browser.stop()` | `browser.close()` |

### Tab

| Python | Rust |
|---|---|
| `tab.evaluate(js, await_promise=True, return_by_value=True)` | `tab.evaluate(js)` / `tab.evaluate_as::<T>(js)` / `tab.evaluate_element(js)` |
| `tab.select(css)` / `tab.select_all(css)` | `tab.select(css)` / `tab.query_all(css)` |
| `tab.query_selector(css)` / `tab.query_selector_all(css)` | `tab.query(css)` / `tab.query_all(css)` |
| `tab.find(text, best_match=True)` / `tab.find_all(text)` | `tab.find(text, true)` / `tab.find_all(text)` |
| `tab.wait_for(selector=..., text=..., timeout=...)`, `tab(...)` | `tab.select_timeout(css, t)` / `tab.find_timeout(text, best_match, t)` |
| `tab.xpath(expr, timeout)` | `tab.xpath(expr, timeout)` |
| `tab.get(url)` | `tab.navigate(url)` |
| `tab.back()` / `tab.forward()` | `tab.back()` / `tab.forward()` (session history; wait for the page) |
| `tab.reload(ignore_cache, script_to_evaluate_on_load)` | `tab.reload()` / `tab.reload_with(ignore_cache, script)` |
| `tab.get_content()` | `tab.content()` |
| `tab.scroll_down(amount)` / `tab.scroll_up(amount)` | `tab.scroll_down(percent)` / `tab.scroll_up(percent)`; `tab.scroll_gesture(x, y, dx, dy)` |
| `tab.scroll_bottom_reached()` | `tab.scroll_bottom_reached()` |
| `tab.save_screenshot(filename, format, full_page)` | `tab.screenshot(ImageFormat, full_page)` / `tab.save_screenshot(path, full_page)` / `tab.screenshot_png()` |
| `tab.get_window()` | `tab.get_window()` |
| `tab.set_window_size(left, top, width, height)` | `tab.set_window_bounds(left, top, width, height)` |
| `tab.set_window_state(state=...)` | `tab.set_window_state(WindowState)` |
| `tab.maximize()` / `minimize()` / `fullscreen()` / `medimize()` | `tab.maximize()` / `minimize()` / `fullscreen()` / `normal()` |
| `tab.activate()` / `tab.bring_to_front()` | `tab.activate()` |
| `tab.close()` | `tab.close()` |
| `tab.mouse_move(x, y)` | `tab.mouse_move(x, y)` |
| `tab.mouse_click(x, y, button, modifiers)` | `tab.mouse_click(x, y)` / `tab.mouse_click_with(x, y, button, modifiers)` |
| `tab.mouse_drag(source, dest, relative, steps)` | `tab.mouse_drag(from, to, steps)` |
| `tab.get_local_storage()` / `tab.set_local_storage(items)` | `tab.local_storage()` / `tab.set_local_storage(items)` |
| `tab.get_frame_tree()` | `tab.frame_tree()` |
| `tab.get_frame_resource_tree()` / `get_frame_resource_urls()` | `tab.frame_resource_tree()` / `tab.frame_resource_urls()` |
| `tab.search_frame_resources(query)` | `tab.send(&cdp::page::SearchInResourceParams::new(..))` per resource |
| `tab.set_download_path(path)` / `tab.download_file(url, filename)` | `tab.set_download_path(path)` / `tab.download_file(url, filename)` |
| `tab.get_all_linked_sources()` / `tab.get_all_urls(absolute)` | `tab.linked_sources()` / `tab.get_all_urls(absolute)` |
| `tab.bypass_insecure_connection_warning()` | `tab.bypass_insecure_connection_warning()` |
| `tab.inspector_url` | `tab.inspector_url()` |
| `tab.send(cdp.x.y(...))`, `tab.feed_cdp(cmd)` | `tab.send(&cdp::x::YParams::new(...))` / `tab.call(method, json)` |
| `tab.add_handler(event, cb)` / `remove_handler` | `tab.subscribe::<Event>()` / `tab.subscribe_raw(method)`; dropping the stream unsubscribes |

### Element

| Python | Rust |
|---|---|
| `el.click()` / `el.mouse_click(button, modifiers)` | `el.click()` / `el.mouse_click()` / `el.mouse_click_with(button, modifiers)` |
| `el.mouse_move()` | `el.mouse_move()` |
| `el.mouse_drag(destination, relative, steps)` | `el.mouse_drag_to(x, y, steps)` / `el.mouse_drag_by(dx, dy, steps)` / `el.mouse_drag_to_element(&other, steps)` |
| `el.send_keys(text)` / `el.clear_input()` / `el.focus()` | `el.send_keys(text)` / `el.clear_input()` / `el.focus()` |
| `el.send_file(*paths)` / `el.select_option()` | `el.send_file(&[paths])` / `el.select_option()` |
| `el.set_value(value)` | `el.set_value(value)` (native `value` setter plus `input`/`change` events) |
| `el.set_text(value)` | `el.set_text(value)` |
| `el.text` / `el.text_all` | `el.text()` (`innerText`) / `el.text_all()` |
| `el.get_html()` | `el.outer_html()` |
| `el.attrs`, `el.attrs.x = v` + `el.save_to_dom()` | `el.attributes()`, `el.attribute(name)`, `el.set_attribute(name, value)`, `el.remove_attribute(name)` |
| `el.tag` / `el.tag_name` | `el.tag()` |
| `el.parent` / `el.children` | `el.parent()` / `el.children()` |
| `el.remove_from_dom()` | `el.remove()` |
| `el.query_selector(css)` / `el.query_selector_all(css)` | `el.query(css)` / `el.query_all(css)` |
| `el.apply(js_function)` / `el(method)` | `el.apply(js_function)` / `el.call_method(name)` |
| `el.get_position(abs=...)` | `el.rect()` / `el.document_rect()` |
| `el.scroll_into_view()` | `el.scroll_into_view()` |
| (none) | `el.scroll_down(percent)` / `el.scroll_up(percent)`: scroll gesture at the element's center, for lists with their own scrollbar |
| `el.save_screenshot(filename, format, scale)` | `el.screenshot(ImageFormat, scale)` / `el.save_screenshot(path)` |

### Not ported

| Python | Reason |
|---|---|
| `tab.open_external_inspector()` / `inspector_open()` | Opens the system browser; `tab.inspector_url()` gives the URL. |
| `el.flash()`, `el.highlight_overlay()`, `tab.flash_point()`, `flash=` arguments | Visual debugging aids drawn into the page. |
| `el.record_video()` / `el.is_recording()` | Records a `<video>` element with `MediaRecorder` into a download. |
| `tab.verify_cf()` / `tab.template_location()` / `util.get_cf_template()` | Template matching on screenshots with OpenCV. |
| `tab.js_dumps()` / `el.get_js_attributes()` | Generic serialization of JS objects; `evaluate_as` / `apply_as` with an explicit expression covers it. |
| `browser.tile_windows()` | Arranges windows on the screen; `tab.set_window_bounds` places each one. |
| `HTTPApi` | Wrapper for the `/json` HTTP endpoints; the crate uses them internally (`Browser::connect`). |
| `util.ProxyForwarder`, proxies with credentials in `create_context` | Local proxy server that forwards to an authenticated upstream proxy. `create_context` takes proxies without credentials. |
| `tab.get_frames()` / `IFrame` | Out-of-process iframes are off with the default flags (`site-per-process` disabled); `browser.tab(target_id)` attaches to any target. |
| `util.filter_recurse*`, `flatten_frame_tree*`, `html_from_tree`, `remove_from_tree`, `el.update()`, `el.tree`, `el.node` and the `Node` properties | Work on a local copy of the DOM tree. An `Element` here is a live handle; queries run in the page. |
| `util.create_from_undetected_chromedriver`, `util.deconstruct_browser`, registered instances | Python process management; `Browser::close` stops the browser and deletes the temporary profile. |
| Packed `.crx` extensions in `add_extension` | Pass the unpacked directory. |

## Differences

- One websocket per browser; tabs use flattened sessions (`Target.attachToTarget { flatten: true }`).
- Domains are enabled only when needed. `Page.enable` is sent before the first navigation of a tab;
  `Runtime.enable` is never sent.
- `--remote-debugging-port=0` is never used: with it, Edge reports `navigator.webdriver === true`.
  A free port is picked before launching, as nodriver does.
- `Config::lang` sets `--lang` and `--accept-lang` (nodriver stores `lang` but does not pass it).
- Packed `.crx` extensions are not unpacked; pass extension directories.
- A profile held by a running browser fails with `Error::ProfileInUse` instead of handing the launch
  over to that browser.
- Microsoft Edge is used when Chrome is not installed.
- The websocket reconnects on its own while the browser runs: tabs get new sessions and keep
  working, per-tab and browser-wide subscriptions keep receiving events, and a
  `Nodriver.reconnected` event is delivered. Commands in flight when the link dropped fail with
  `Error::Disconnected` and are not resent. Domains enabled by the caller (`Network.enable`, ...)
  must be enabled again. `Browser::close` turns reconnection off.
- `back`/`forward` go through `Page.navigateToHistoryEntry` and wait for the page (nodriver runs
  `history.back()` and returns). `navigate` to a fragment of the same document waits for the
  history entry to commit.
- `scroll_bottom_reached` compares `scrollTop + clientHeight` of the scrolling element with its
  `scrollHeight` (nodriver uses `document.body.offsetHeight`, which is off on pages whose body does
  not hold the height).
- `type_text`/`send_keys` set `key` on the `char` events, so pages see `keypress` with
  `event.key` and `keyCode` filled in.
- Cookies are saved as JSON (a list of `Network.Cookie`) instead of a pickle, and filtered by a
  closure instead of a regex. nodriver's `save` ignores its pattern and writes every cookie.
- `xpath` with an invalid expression returns `Error::Js`; nodriver returns an empty list.
- `Element::set_value` sets the `value` property and fires `input`/`change`; nodriver's
  `DOM.setNodeValue` only changes text nodes, which is `set_text` here.
- `text_all` trims each text node and skips the empty ones.

## CDP types

`nodriver-cdp` holds the protocol types, generated by `cdp-gen` from the official protocol JSON
(`crates/nodriver-cdp/protocol`). Only the domains used by this crate and the ones they reference
are generated: Browser, DOM, DOMStorage, Debugger, Emulation, Fetch, IO, Input, Log, Network, Page,
Runtime, Security, Storage, Target. To regenerate:

```
cargo run -p cdp-gen && cargo fmt -p nodriver-cdp
```

## Tests

`cargo test -p nodriver` launches headless browsers with temporary profiles against local test
pages. `NODRIVER_HEADFUL=1` shows the windows.

## License

AGPL-3.0, same as nodriver.
