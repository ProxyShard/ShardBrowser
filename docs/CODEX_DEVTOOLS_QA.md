# Codex ShardBrowser + Chrome DevTools QA

Use this when a Codex task needs both a ShardX profile and DevTools-grade
console/network/performance evidence.

## Current attach audit

- `shardbrowser` MCP starts the chosen ShardX profile and returns CDP data with
  `devtools_context`.
- `chrome-devtools-mcp@1.5.0` supports `--browserUrl` and `--wsEndpoint`, but
  the exposed Codex tools do not include a runtime attach/connect call. Pick the
  browser in the MCP client config before startup.
- Keep this integration at the MCP/client layer. Do not wire
  `chrome_devtools` into Launcher core or browser fingerprint code.

## Workflow

1. Resolve the profile:
   ```text
   mcp__shardbrowser.list_profiles
   ```
2. Open the target page in the intended profile:
   ```text
   mcp__shardbrowser.browser_navigate({
     "profile_id": "<profile-id>",
     "url": "https://example.com/"
   })
   ```
3. Get the live CDP handoff:
   ```text
   mcp__shardbrowser.devtools_context({
     "profile_id": "<profile-id>"
   })
   ```
   If the profile is already running without CDP, stop and restart it through
   ShardBrowser MCP or the Automation API; a running browser process cannot get
   a CDP port retrofitted in place.
4. For ShardX-only checks, continue with `shardbrowser` tools:
   `browser_aria_snapshot`, `browser_screenshot`, `browser_capture_start`,
   `browser_capture_stop`, and `browser_set_network_conditions`.
5. For Chrome DevTools MCP checks against the ShardX page, register a second
   MCP entry from the returned CDP URL, then restart Codex:
   ```powershell
   codex mcp add shardbrowser-devtools -- cmd /c npx -y chrome-devtools-mcp@1.5.0 --browserUrl http://127.0.0.1:<cdp-port> --no-usage-statistics --no-performance-crux --redactNetworkHeaders
   ```
6. After restart, use the configured DevTools namespace for `take_snapshot`,
   `list_console_messages`, `list_network_requests`, `performance_start_trace`,
   `performance_stop_trace`, and `lighthouse_audit`.

## End-to-end prompt goal

```text
Use ShardBrowser MCP to open the requested URL in the exact profile, call
devtools_context, and report the CDP http_url plus current page title/url. If
chrome_devtools is still configured as isolated Chrome, do not claim it audited
the ShardX profile; either run ShardBrowser-native screenshot, a11y, and
network checks, or give the exact Codex MCP add/repair command needed to
register chrome-devtools-mcp with the returned --browserUrl. Never print tokens,
cookies, fingerprint payloads, or proxy credentials.
```
