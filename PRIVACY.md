# Privacy Policy for Terminaler

No data about your device or Terminaler usage leaves your device by default.

## Data Stored Locally

Terminaler stores the following data on your local machine:

- **Configuration** — Your settings in `%APPDATA%\Terminaler\terminaler.json`
- **Session state** — Tab and pane layout for session restore, saved in `%APPDATA%\Terminaler\sessions/`
- **Scrollback buffer** — Terminal output is held in memory. It is not written to disk or shared.
- **Web access token** — If remote web access is enabled, an authentication token is stored in `%APPDATA%\Terminaler\web-token`

All local files are scoped to your user profile and are not accessible to other users on the system.

## Remote Web Access

If you enable the optional web access feature (`webAccess.enabled` in config), Terminaler runs a local web server that allows browser-based access to your terminal sessions over your network.

- **Disabled by default** — No network listener is started unless you opt in
- **Token authentication** — A randomly generated token is required to connect
- **LAN-only by default** — Binds to `127.0.0.1:9876`; you must explicitly configure `0.0.0.0` to allow LAN access

## No Telemetry

Terminaler does not phone home, check for updates, or send any data to external servers. There is no analytics, crash reporting, or usage tracking.

## Third-Party Builds

The above applies to the Terminaler source code and binaries built from this repository. If you obtained a pre-built binary from another source, it may have been modified.
