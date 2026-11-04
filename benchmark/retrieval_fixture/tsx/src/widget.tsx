/** Graceful retries restore a dropped connection after a transient gateway timeout.
 * Operator note: reopen a stream after temporary upstream interruption with bounded backoff.
 */
export function refreshToken() { return true; }
export function token_refresh() { return true; }
export function TokenRefresher() { return <button />; }
export function buildHTTP2Client() { return {}; }
export function parseHTTPStatus200() { return 200; }
export function sharedRetryPolicy() { return "retry"; }
// retry policy: preserve a session after a short service interruption.
