// Opt-in diagnostics ("Log engine timings" setting): one place that talks to the console, at
// the debug level (hidden unless the devtools console shows verbose output).

let enabled = false;

/** Called by the plugin whenever the setting changes. */
export function setDebugLogging(on: boolean): void {
	enabled = on;
}

/** Logs when the setting is on; silent otherwise. */
export function debugLog(...args: unknown[]): void {
	if (enabled) console.debug('[ff10k]', ...args);
}
